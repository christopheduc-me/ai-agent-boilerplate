//! Inbound HTTP adapter (Axum): routes, DTOs, auth extractor, rate limiting,
//! request correlation, SSE job updates.

pub mod rate_limit;
pub mod request_id;
pub mod sse;

use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::application::answer_clarification::AnswerError;
use crate::application::ingest_results::IngestError;
use crate::application::launch_search::LaunchError;
use crate::application::login_user::LoginError;
use crate::application::recurring_searches::RecurringError;
use crate::application::refresh_session::RefreshError;
use crate::application::register_user::RegisterError;
use crate::application::{
    AnswerClarification, IngestResults, LaunchSearch, LoginUser, RecurringSearches, RefreshSession,
    RegisterUser, SearchQueries, SessionTokens,
};
use crate::domain::ports::{
    JobDispatcher, JobRepository, PasswordHasher, RecurringSearchRepository,
    RefreshTokenRepository, TokenService, UserRepository,
};
use crate::domain::{AgentStep, JobMode, JobStatus, RecurringSearch, ResearchJob, SearchResult};

/// Name of the HttpOnly cookie carrying the refresh token (ADR-008).
const REFRESH_COOKIE: &str = "refresh_token";

#[derive(Clone)]
pub struct AppState {
    register: Arc<RegisterUser>,
    login: Arc<LoginUser>,
    refresh: Arc<RefreshSession>,
    launch: Arc<LaunchSearch>,
    answer: Arc<AnswerClarification>,
    recurring: Arc<RecurringSearches>,
    ingest: Arc<IngestResults>,
    queries: Arc<SearchQueries>,
    tokens: Arc<dyn TokenService>,
    internal_token: String,
    refresh_ttl_days: i64,
}

/// HTTP throttling knobs (ADR-017). Internal routes are never rate limited.
#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    /// Per-IP limit on `/api/auth/*` (brute-force protection).
    pub auth_per_minute: u32,
    /// Per-IP limit on the rest of `/api/*`.
    pub api_per_minute: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            auth_per_minute: 10,
            api_per_minute: 120,
        }
    }
}

impl AppState {
    #[allow(clippy::too_many_arguments)] // boilerplate wiring point, one call site per binary
    pub fn new(
        users: Arc<dyn UserRepository>,
        jobs: Arc<dyn JobRepository>,
        refresh_tokens: Arc<dyn RefreshTokenRepository>,
        recurring: Arc<dyn RecurringSearchRepository>,
        dispatcher: Arc<dyn JobDispatcher>,
        hasher: Arc<dyn PasswordHasher>,
        tokens: Arc<dyn TokenService>,
        internal_token: String,
        daily_search_quota: u32,
        refresh_ttl_days: i64,
    ) -> Self {
        Self {
            register: Arc::new(RegisterUser::new(users.clone(), hasher.clone())),
            login: Arc::new(LoginUser::new(
                users,
                hasher,
                tokens.clone(),
                refresh_tokens.clone(),
                refresh_ttl_days,
            )),
            refresh: Arc::new(RefreshSession::new(
                refresh_tokens,
                tokens.clone(),
                refresh_ttl_days,
            )),
            launch: Arc::new(LaunchSearch::new(
                jobs.clone(),
                dispatcher.clone(),
                daily_search_quota,
            )),
            answer: Arc::new(AnswerClarification::new(jobs.clone(), dispatcher)),
            recurring: Arc::new(RecurringSearches::new(recurring)),
            ingest: Arc::new(IngestResults::new(jobs.clone())),
            queries: Arc::new(SearchQueries::new(jobs)),
            tokens,
            internal_token,
            refresh_ttl_days,
        }
    }
}

pub fn router(state: AppState) -> Router {
    router_with_limits(state, RateLimitConfig::default())
}

pub fn router_with_limits(state: AppState, limits: RateLimitConfig) -> Router {
    let auth_limiter = rate_limit::FixedWindowLimiter::per_minute(limits.auth_per_minute);
    let api_limiter = rate_limit::FixedWindowLimiter::per_minute(limits.api_per_minute);

    let auth_routes = Router::new()
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/refresh", post(refresh))
        .route("/api/auth/logout", post(logout))
        .layer(axum::middleware::from_fn_with_state(
            auth_limiter,
            rate_limit::rate_limit,
        ));

    let api_routes = Router::new()
        .route("/api/searches", post(create_search).get(list_searches))
        .route("/api/searches/{id}", get(get_search))
        .route("/api/searches/{id}/answer", post(answer_search))
        .route("/api/searches/{id}/events", get(search_events))
        .route("/api/recurring", post(create_recurring).get(list_recurring))
        .route(
            "/api/recurring/{id}",
            axum::routing::delete(delete_recurring),
        )
        .layer(axum::middleware::from_fn_with_state(
            api_limiter,
            rate_limit::rate_limit,
        ));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(auth_routes)
        .merge(api_routes)
        .route("/internal/jobs/{id}/started", post(internal_started))
        .route("/internal/jobs/{id}/results", post(internal_results))
        .route("/internal/jobs/{id}/steps", post(internal_step))
        .route("/internal/jobs/{id}/question", post(internal_question))
        .route("/internal/jobs/{id}/failure", post(internal_failure))
        // Outermost layer: every request gets a correlation span (ADR-018).
        .layer(axum::middleware::from_fn(request_id::request_id))
        .with_state(state)
}

fn error_body(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

// ---------------------------------------------------------------- auth extractor

/// Extracts the authenticated user id from the `Authorization: Bearer <jwt>` header.
pub struct AuthUser(pub Uuid);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user_id = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|token| state.tokens.verify(token))
            .ok_or_else(|| {
                error_body(StatusCode::UNAUTHORIZED, "invalid or missing access token")
            })?;
        Ok(AuthUser(user_id))
    }
}

// ---------------------------------------------------------------- refresh cookie helpers

/// `Set-Cookie` value for the refresh token: HttpOnly + Secure + SameSite=Strict,
/// scoped to the auth endpoints only (ADR-008). Browsers exempt localhost from
/// the Secure requirement, so development over http keeps working.
fn refresh_cookie(value: &str, max_age_seconds: i64) -> String {
    format!(
        "{REFRESH_COOKIE}={value}; HttpOnly; Secure; SameSite=Strict; Path=/api/auth; Max-Age={max_age_seconds}"
    )
}

fn clear_refresh_cookie() -> String {
    refresh_cookie("", 0)
}

/// Extracts the refresh token from the `Cookie` header.
fn read_refresh_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == REFRESH_COOKIE)
        .map(|(_, value)| value.to_string())
        .filter(|v| !v.is_empty())
}

fn session_response(state: &AppState, tokens: SessionTokens) -> Response {
    let cookie = refresh_cookie(&tokens.refresh_token, state.refresh_ttl_days * 86_400);
    (
        [("set-cookie", cookie)],
        Json(json!({ "access_token": tokens.access_token, "token_type": "Bearer" })),
    )
        .into_response()
}

/// Returns a rejection response when the internal token is missing or wrong.
fn check_internal_token(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let provided = headers
        .get("x-internal-token")
        .and_then(|v| v.to_str().ok());
    if provided != Some(state.internal_token.as_str()) {
        return Some(error_body(
            StatusCode::UNAUTHORIZED,
            "invalid or missing internal token",
        ));
    }
    None
}

// ---------------------------------------------------------------- DTOs

#[derive(Deserialize)]
struct CredentialsRequest {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct CreateSearchRequest {
    keyword: String,
    // Workflow (fixed pipeline) or agent (decision loop, ADR-030); defaulted
    // so pre-ADR-030 clients keep working.
    #[serde(default)]
    mode: JobMode,
}

#[derive(Serialize)]
struct JobView {
    id: Uuid,
    keyword: String,
    mode: JobMode,
    status: JobStatus,
    error: Option<String>,
    // Clarification dialog (ADR-032), null until the agent asks / the user answers.
    question: Option<String>,
    answer: Option<String>,
    // Set on scheduler-launched runs (ADR-033).
    recurring_search_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl From<&ResearchJob> for JobView {
    fn from(job: &ResearchJob) -> Self {
        Self {
            id: job.id,
            keyword: job.keyword.clone(),
            mode: job.mode,
            status: job.status,
            error: job.error.clone(),
            question: job.question.clone(),
            answer: job.answer.clone(),
            recurring_search_id: job.recurring_search_id,
            created_at: job.created_at,
            completed_at: job.completed_at,
        }
    }
}

#[derive(Deserialize)]
struct CreateRecurringRequest {
    keyword: String,
    #[serde(default)]
    mode: JobMode,
    interval_minutes: u32,
}

#[derive(Serialize)]
struct RecurringView {
    id: Uuid,
    keyword: String,
    mode: JobMode,
    interval_minutes: u32,
    created_at: DateTime<Utc>,
    last_run_at: Option<DateTime<Utc>>,
}

impl From<&RecurringSearch> for RecurringView {
    fn from(search: &RecurringSearch) -> Self {
        Self {
            id: search.id,
            keyword: search.keyword.clone(),
            mode: search.mode,
            interval_minutes: search.interval_minutes,
            created_at: search.created_at,
            last_run_at: search.last_run_at,
        }
    }
}

#[derive(Deserialize)]
struct ResultsRequest {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct FailureRequest {
    error: String,
}

#[derive(Deserialize)]
struct QuestionRequest {
    question: String,
}

#[derive(Deserialize)]
struct AnswerRequest {
    answer: String,
}

// ---------------------------------------------------------------- public handlers

async fn register(State(state): State<AppState>, Json(body): Json<CredentialsRequest>) -> Response {
    match state.register.execute(&body.email, &body.password).await {
        Ok(user) => (
            StatusCode::CREATED,
            Json(json!({ "id": user.id, "email": user.email })),
        )
            .into_response(),
        Err(RegisterError::EmailTaken) => {
            error_body(StatusCode::CONFLICT, "email already registered")
        }
        Err(e @ (RegisterError::InvalidEmail | RegisterError::PasswordTooShort)) => {
            error_body(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string())
        }
        Err(RegisterError::Infrastructure(e)) => {
            tracing::error!(error = %e, "register failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn login(State(state): State<AppState>, Json(body): Json<CredentialsRequest>) -> Response {
    match state.login.execute(&body.email, &body.password).await {
        Ok(tokens) => session_response(&state, tokens),
        Err(LoginError::InvalidCredentials) => {
            error_body(StatusCode::UNAUTHORIZED, "invalid credentials")
        }
        Err(LoginError::Infrastructure(e)) => {
            tracing::error!(error = %e, "login failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Rotates the refresh cookie and returns a fresh access token (ADR-008).
async fn refresh(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(presented) = read_refresh_cookie(&headers) else {
        return error_body(StatusCode::UNAUTHORIZED, "missing refresh token");
    };
    match state.refresh.rotate(&presented).await {
        Ok(tokens) => session_response(&state, tokens),
        Err(RefreshError::InvalidToken) => (
            [("set-cookie", clear_refresh_cookie())],
            error_body(StatusCode::UNAUTHORIZED, "invalid or expired refresh token"),
        )
            .into_response(),
        Err(RefreshError::Infrastructure(e)) => {
            tracing::error!(error = %e, "refresh failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Revokes the refresh token and clears the cookie. Always succeeds (idempotent).
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(presented) = read_refresh_cookie(&headers) {
        if let Err(e) = state.refresh.revoke(&presented).await {
            tracing::error!(error = %e, "logout revocation failed");
        }
    }
    (
        StatusCode::NO_CONTENT,
        [("set-cookie", clear_refresh_cookie())],
    )
        .into_response()
}

async fn create_search(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CreateSearchRequest>,
) -> Response {
    match state
        .launch
        .execute(user_id, &body.keyword, body.mode)
        .await
    {
        Ok(job) => (StatusCode::ACCEPTED, Json(json!({ "job_id": job.id }))).into_response(),
        Err(LaunchError::InvalidJob(e)) => {
            error_body(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string())
        }
        Err(e @ LaunchError::QuotaExceeded(_)) => {
            error_body(StatusCode::TOO_MANY_REQUESTS, &e.to_string())
        }
        Err(LaunchError::DispatchFailed) => {
            error_body(StatusCode::BAD_GATEWAY, "failed to reach the agent")
        }
        Err(LaunchError::Infrastructure(e)) => {
            tracing::error!(error = %e, "launch failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn list_searches(State(state): State<AppState>, AuthUser(user_id): AuthUser) -> Response {
    match state.queries.list(user_id).await {
        Ok(jobs) => Json(jobs.iter().map(JobView::from).collect::<Vec<_>>()).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list searches failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

// ---------------------------------------------------------------- recurring searches (ADR-033)

async fn create_recurring(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CreateRecurringRequest>,
) -> Response {
    match state
        .recurring
        .create(user_id, &body.keyword, body.mode, body.interval_minutes)
        .await
    {
        Ok(search) => (StatusCode::CREATED, Json(RecurringView::from(&search))).into_response(),
        Err(e @ RecurringError::Invalid(_)) => {
            error_body(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string())
        }
        Err(e @ RecurringError::TooMany(_)) => {
            error_body(StatusCode::TOO_MANY_REQUESTS, &e.to_string())
        }
        Err(RecurringError::NotFound) => error_body(StatusCode::NOT_FOUND, "not found"),
        Err(RecurringError::Infrastructure(e)) => {
            tracing::error!(error = %e, "create recurring search failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn list_recurring(State(state): State<AppState>, AuthUser(user_id): AuthUser) -> Response {
    match state.recurring.list(user_id).await {
        Ok(searches) => {
            Json(searches.iter().map(RecurringView::from).collect::<Vec<_>>()).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "list recurring searches failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn delete_recurring(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<Uuid>,
) -> Response {
    match state.recurring.delete(user_id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(RecurringError::NotFound) => {
            error_body(StatusCode::NOT_FOUND, "recurring search not found")
        }
        Err(e) => {
            tracing::error!(error = %e, "delete recurring search failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// The user answers the agent's clarification question (ADR-032).
async fn answer_search(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(job_id): Path<Uuid>,
    Json(body): Json<AnswerRequest>,
) -> Response {
    match state.answer.execute(user_id, job_id, &body.answer).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(AnswerError::NotFound) => error_body(StatusCode::NOT_FOUND, "search not found"),
        Err(AnswerError::InvalidAnswer(crate::domain::job::JobError::NotAwaitingInput)) => {
            error_body(StatusCode::CONFLICT, "search is not awaiting an answer")
        }
        Err(AnswerError::InvalidAnswer(e)) => {
            error_body(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string())
        }
        Err(AnswerError::DispatchFailed) => {
            error_body(StatusCode::BAD_GATEWAY, "failed to reach the agent")
        }
        Err(AnswerError::Infrastructure(e)) => {
            tracing::error!(error = %e, "answer clarification failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// The job detail payload, shared by `GET /api/searches/{id}` and the SSE
/// stream (ADR-026) so both surfaces always carry the same shape.
pub(crate) fn job_detail_json(
    job: &ResearchJob,
    results: &[SearchResult],
    steps: &[AgentStep],
) -> serde_json::Value {
    let mut body = serde_json::to_value(JobView::from(job)).expect("serializable view");
    body["results"] = serde_json::to_value(results).expect("serializable results");
    // The agent journal (ADR-030); always present, empty in workflow mode.
    body["steps"] = serde_json::to_value(steps).expect("serializable steps");
    body
}

async fn get_search(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(job_id): Path<Uuid>,
) -> Response {
    match state.queries.get(user_id, job_id).await {
        Ok(Some((job, results, steps))) => {
            Json(job_detail_json(&job, &results, &steps)).into_response()
        }
        Ok(None) => error_body(StatusCode::NOT_FOUND, "search not found"),
        Err(e) => {
            tracing::error!(error = %e, "get search failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// SSE stream of job updates (ADR-026): an `update` event per change, closed
/// after the terminal status. The client keeps polling as a fallback.
async fn search_events(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(job_id): Path<Uuid>,
) -> Response {
    // Reject unknown/foreign jobs with a proper 404 before streaming.
    match state.queries.get(user_id, job_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return error_body(StatusCode::NOT_FOUND, "search not found"),
        Err(e) => {
            tracing::error!(error = %e, "search events failed");
            return error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    }
    let stream = sse::job_updates(state.queries.clone(), user_id, job_id);
    axum::response::sse::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

// ---------------------------------------------------------------- internal handlers (worker -> backend, ADR-006)

async fn internal_started(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    if let Some(rejection) = check_internal_token(&state, &headers) {
        return rejection;
    }
    match state.ingest.start(job_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(IngestError::JobNotFound) => error_body(StatusCode::NOT_FOUND, "job not found"),
        Err(IngestError::Infrastructure(e)) => {
            tracing::error!(error = %e, "mark job started failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn internal_results(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ResultsRequest>,
) -> Response {
    if let Some(rejection) = check_internal_token(&state, &headers) {
        return rejection;
    }
    match state.ingest.complete(job_id, &body.results).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(IngestError::JobNotFound) => error_body(StatusCode::NOT_FOUND, "job not found"),
        Err(IngestError::Infrastructure(e)) => {
            tracing::error!(error = %e, "ingest results failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Records one agent-loop decision for the live journal (ADR-030).
async fn internal_step(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(step): Json<AgentStep>,
) -> Response {
    if let Some(rejection) = check_internal_token(&state, &headers) {
        return rejection;
    }
    match state.ingest.record_step(job_id, &step).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(IngestError::JobNotFound) => error_body(StatusCode::NOT_FOUND, "job not found"),
        Err(IngestError::Infrastructure(e)) => {
            tracing::error!(error = %e, "record agent step failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// The agent asked the user a clarification question (ADR-032).
async fn internal_question(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<QuestionRequest>,
) -> Response {
    if let Some(rejection) = check_internal_token(&state, &headers) {
        return rejection;
    }
    match state.ingest.request_input(job_id, &body.question).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(IngestError::JobNotFound) => error_body(StatusCode::NOT_FOUND, "job not found"),
        Err(IngestError::Infrastructure(e)) => {
            tracing::error!(error = %e, "record clarification question failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn internal_failure(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<FailureRequest>,
) -> Response {
    if let Some(rejection) = check_internal_token(&state, &headers) {
        return rejection;
    }
    match state.ingest.fail(job_id, body.error).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(IngestError::JobNotFound) => error_body(StatusCode::NOT_FOUND, "job not found"),
        Err(IngestError::Infrastructure(e)) => {
            tracing::error!(error = %e, "ingest failure failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}
