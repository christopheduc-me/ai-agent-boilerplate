//! Inbound HTTP adapter (Axum): routes, DTOs, auth extractor, rate limiting,
//! request correlation, SSE job updates.

pub mod rate_limit;
pub mod request_id;
pub mod security_headers;
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
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;
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
    DigestSender, JobDispatcher, JobRepository, PasswordHasher, RecurringSearchRepository,
    RefreshTokenRepository, SecurityAudit, TokenService, UserRepository,
};
use crate::domain::{
    AgentStep, DateConfidence, EventType, JobMode, JobStatus, JobUsage, RecurringSearch,
    ResearchJob, SearchResult, SecurityEvent, SecurityEventKind,
};

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
    /// Security audit log (ADR-057): failed/throttled logins, quota hits.
    audit: Arc<dyn SecurityAudit>,
    /// Per-account login throttle (ADR-057), keyed by email — independent of
    /// the per-IP limiter, so credential-stuffing across many IPs is capped too.
    login_throttle: rate_limit::Limiter,
    internal_token: String,
    refresh_ttl_days: i64,
}

/// HTTP throttling knobs (ADR-017). Internal routes are never rate limited.
#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    /// Per-IP limit on `/api/auth/*` (brute-force protection).
    pub auth_per_minute: u32,
    /// Per-IP limit on the rest of `/api/*`.
    pub api_per_minute: u32,
    /// Per-**account** limit on login attempts (ADR-057): email-keyed, so a
    /// credential-stuffing run spread over many IPs is throttled per target.
    pub login_per_minute: u32,
    /// Redis-backed distributed limiting (ADR-037): set when the backend
    /// scales horizontally; None keeps the in-memory limiter.
    pub redis_url: Option<String>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            auth_per_minute: 10,
            api_per_minute: 120,
            login_per_minute: 10,
            redis_url: None,
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
        digests: Arc<dyn DigestSender>,
        hasher: Arc<dyn PasswordHasher>,
        tokens: Arc<dyn TokenService>,
        audit: Arc<dyn SecurityAudit>,
        login_throttle: rate_limit::Limiter,
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
                audit.clone(),
                refresh_ttl_days,
            )),
            launch: Arc::new(LaunchSearch::new(
                jobs.clone(),
                dispatcher.clone(),
                daily_search_quota,
            )),
            answer: Arc::new(AnswerClarification::new(jobs.clone(), dispatcher)),
            recurring: Arc::new(RecurringSearches::new(recurring.clone())),
            ingest: Arc::new(IngestResults::new(jobs.clone(), recurring, digests)),
            queries: Arc::new(SearchQueries::new(jobs)),
            tokens,
            audit,
            login_throttle,
            internal_token,
            refresh_ttl_days,
        }
    }
}

// ---------------------------------------------------------------- OpenAPI (ADR-049 amendment)

/// `{ "job_id": "…" }` — a launched search.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
struct JobCreatedResponse {
    job_id: Uuid,
}

/// `{ "access_token": "…" }` — the short-lived bearer; the refresh token is set
/// as an HttpOnly cookie (ADR-008), not in the body.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
struct AccessTokenResponse {
    access_token: String,
}

/// `{ "id": "…", "email": "…" }` — a created account.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
struct AccountResponse {
    id: Uuid,
    email: String,
}

/// `{ "error": "…" }` — the uniform error body (ADR-018).
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
struct ErrorResponse {
    error: String,
}

/// Declares the `bearer` (JWT) auth scheme referenced by the protected paths.
struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
        }
    }
}

/// The public HTTP API (ADR-049 amendment). This documents only the public
/// surface (Vue → Rust); the `/internal/*` worker callbacks (ADR-006) are pinned
/// by the contract fixtures instead. The contract's source of truth stays the
/// zod schemas + fixtures (ADR-049) — this is browsable documentation, served
/// self-hosted at `/api/docs`, with the raw spec at `/api/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    modifiers(&SecurityAddon),
    info(
        title = "AI agent boilerplate — public API",
        description = "The browser-facing API (auth, searches, recurring searches). \
                       Contract pinned by fixtures + zod (ADR-049).",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "MIT"),
    ),
    paths(
        register,
        login,
        refresh,
        logout,
        create_search,
        list_searches,
        get_search,
        answer_search,
        create_recurring,
        list_recurring,
        delete_recurring,
    ),
    components(schemas(
        CredentialsRequest,
        CreateSearchRequest,
        CreateRecurringRequest,
        AnswerRequest,
        JobView,
        JobDetailView,
        RecurringView,
        JobCreatedResponse,
        AccessTokenResponse,
        AccountResponse,
        ErrorResponse,
        SearchResult,
        AgentStep,
        JobUsage,
        JobStatus,
        JobMode,
        EventType,
        DateConfidence,
    )),
    tags(
        (name = "auth", description = "Registration, login, session refresh (ADR-008)"),
        (name = "searches", description = "Launch and read searches (ADR-030/032)"),
        (name = "recurring", description = "Scheduled recurring searches (ADR-033)"),
    ),
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    router_with_limits(state, RateLimitConfig::default())
}

pub fn router_with_limits(state: AppState, limits: RateLimitConfig) -> Router {
    let redis_url = limits.redis_url.as_deref();
    let auth_limiter = rate_limit::Limiter::per_minute(limits.auth_per_minute, "auth", redis_url);
    let api_limiter = rate_limit::Limiter::per_minute(limits.api_per_minute, "api", redis_url);

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
        // Interactive API docs (ADR-049 amendment): Swagger UI at /api/docs,
        // raw spec at /api/openapi.json. Assets are vendored (self-hosted).
        .merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", ApiDoc::openapi()))
        .merge(auth_routes)
        .merge(api_routes)
        .route("/internal/jobs/{id}/started", post(internal_started))
        .route("/internal/jobs/{id}/results", post(internal_results))
        .route("/internal/jobs/{id}/steps", post(internal_step))
        .route("/internal/jobs/{id}/question", post(internal_question))
        .route("/internal/jobs/{id}/usage", post(internal_usage))
        .route("/internal/jobs/{id}/failure", post(internal_failure))
        // Correlation span (ADR-018) around every request.
        .layer(axum::middleware::from_fn(request_id::request_id))
        // Outermost: security headers on every response, errors included (ADR-054).
        .layer(axum::middleware::from_fn(
            security_headers::security_headers,
        ))
        .with_state(state)
}

fn error_body(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Best-effort client IP for the audit log (ADR-057): the first
/// `X-Forwarded-For` entry set by the trusted proxy (ADR-014/015), like the
/// rate limiter keys on. `None` when the header is absent (e.g. tests).
fn client_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
}

/// Records a security event (ADR-057). Always logs a structured line so it
/// surfaces in the observability pillars (ADR-018/050) even if the DB write
/// fails; the persistence itself is best-effort and never breaks the request.
async fn record_security_event(state: &AppState, event: SecurityEvent) {
    tracing::warn!(
        security_event = %event.kind,
        user_id = ?event.user_id,
        client_ip = ?event.client_ip,
        detail = %event.detail,
        "security event"
    );
    if let Err(e) = state.audit.record(&event).await {
        tracing::error!(error = %e, "failed to record security event");
    }
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
/// Constant-time byte comparison (ADR-055): the loop runs over the whole input
/// regardless of where a mismatch is, so the compare time does not leak how much
/// of a guessed token was correct. The length is allowed to short-circuit (it is
/// not the secret). Mirrors the constant-time check the HMAC path already uses.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn check_internal_token(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let matches = headers
        .get("x-internal-token")
        .map(|v| constant_time_eq(v.as_bytes(), state.internal_token.as_bytes()))
        .unwrap_or(false);
    if !matches {
        return Some(error_body(
            StatusCode::UNAUTHORIZED,
            "invalid or missing internal token",
        ));
    }
    None
}

// ---------------------------------------------------------------- DTOs

#[derive(Deserialize, ToSchema)]
struct CredentialsRequest {
    email: String,
    password: String,
}

#[derive(Deserialize, ToSchema)]
struct CreateSearchRequest {
    keyword: String,
    // Workflow (fixed pipeline) or agent (decision loop, ADR-030); defaulted
    // so pre-ADR-030 clients keep working.
    #[serde(default)]
    mode: JobMode,
}

#[derive(Serialize, ToSchema)]
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
    // Accumulated API spend (ADR-038).
    usage: JobUsage,
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
            usage: job.usage,
            created_at: job.created_at,
            completed_at: job.completed_at,
        }
    }
}

#[derive(Deserialize, ToSchema)]
struct CreateRecurringRequest {
    keyword: String,
    #[serde(default)]
    mode: JobMode,
    interval_minutes: u32,
    /// Digest target (ADR-036): notified when a run finds new results.
    #[serde(default)]
    webhook_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct RecurringView {
    id: Uuid,
    keyword: String,
    mode: JobMode,
    interval_minutes: u32,
    webhook_url: Option<String>,
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
            webhook_url: search.webhook_url.clone(),
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

#[derive(Deserialize, ToSchema)]
struct AnswerRequest {
    answer: String,
}

// ---------------------------------------------------------------- public handlers

#[utoipa::path(post, path = "/api/auth/register", tag = "auth",
    request_body = CredentialsRequest,
    responses(
        (status = 201, description = "Account created", body = AccountResponse),
        (status = 409, description = "Email already registered", body = ErrorResponse),
        (status = 422, description = "Invalid credentials", body = ErrorResponse)))]
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

#[utoipa::path(post, path = "/api/auth/login", tag = "auth",
    request_body = CredentialsRequest,
    responses(
        (status = 200, description = "Access token in body; refresh token set as an HttpOnly cookie (ADR-008)", body = AccessTokenResponse),
        (status = 401, description = "Invalid email or password", body = ErrorResponse)))]
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CredentialsRequest>,
) -> Response {
    // Per-account throttle (ADR-057): keyed by the normalized email so an
    // attacker cannot dodge it with IP rotation or case tricks. Checked before
    // the (deliberately costly) argon2 verify, so it also sheds that load.
    let email_key = body.email.trim().to_lowercase();
    let ip = client_ip(&headers);
    if !state.login_throttle.allow(&email_key).await {
        record_security_event(
            &state,
            SecurityEvent::new(SecurityEventKind::LoginThrottled, None, ip, email_key),
        )
        .await;
        return error_body(
            StatusCode::TOO_MANY_REQUESTS,
            "too many login attempts, slow down",
        );
    }

    match state.login.execute(&body.email, &body.password).await {
        Ok(tokens) => session_response(&state, tokens),
        Err(LoginError::InvalidCredentials) => {
            record_security_event(
                &state,
                SecurityEvent::new(SecurityEventKind::LoginFailed, None, ip, email_key),
            )
            .await;
            error_body(StatusCode::UNAUTHORIZED, "invalid credentials")
        }
        Err(LoginError::Infrastructure(e)) => {
            tracing::error!(error = %e, "login failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Rotates the refresh cookie and returns a fresh access token (ADR-008).
#[utoipa::path(post, path = "/api/auth/refresh", tag = "auth",
    responses(
        (status = 200, description = "Rotated refresh cookie, new access token", body = AccessTokenResponse),
        (status = 401, description = "Missing or invalid refresh cookie", body = ErrorResponse)))]
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
#[utoipa::path(post, path = "/api/auth/logout", tag = "auth",
    responses((status = 204, description = "Refresh token revoked, cookie cleared")))]
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

#[utoipa::path(post, path = "/api/searches", tag = "searches",
    security(("bearer" = [])),
    request_body = CreateSearchRequest,
    responses(
        (status = 202, description = "Search launched", body = JobCreatedResponse),
        (status = 422, description = "Empty keyword", body = ErrorResponse),
        (status = 429, description = "Daily quota exceeded (ADR-017)", body = ErrorResponse)))]
async fn create_search(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
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
            record_security_event(
                &state,
                SecurityEvent::new(
                    SecurityEventKind::QuotaExceeded,
                    Some(user_id),
                    client_ip(&headers),
                    "daily search quota",
                ),
            )
            .await;
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

#[utoipa::path(get, path = "/api/searches", tag = "searches",
    security(("bearer" = [])),
    responses((status = 200, description = "The user's searches, newest first", body = [JobView])))]
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

#[utoipa::path(post, path = "/api/recurring", tag = "recurring",
    security(("bearer" = [])),
    request_body = CreateRecurringRequest,
    responses(
        (status = 201, description = "Recurring search created", body = RecurringView),
        (status = 422, description = "Invalid keyword or interval", body = ErrorResponse),
        (status = 429, description = "Too many recurring searches", body = ErrorResponse)))]
async fn create_recurring(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CreateRecurringRequest>,
) -> Response {
    match state
        .recurring
        .create(
            user_id,
            &body.keyword,
            body.mode,
            body.interval_minutes,
            body.webhook_url.as_deref(),
        )
        .await
    {
        Ok(search) => (StatusCode::CREATED, Json(recurring_search_json(&search))).into_response(),
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

#[utoipa::path(get, path = "/api/recurring", tag = "recurring",
    security(("bearer" = [])),
    responses((status = 200, description = "The user's recurring searches", body = [RecurringView])))]
async fn list_recurring(State(state): State<AppState>, AuthUser(user_id): AuthUser) -> Response {
    match state.recurring.list(user_id).await {
        Ok(searches) => Json(
            searches
                .iter()
                .map(recurring_search_json)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list recurring searches failed");
            error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

#[utoipa::path(delete, path = "/api/recurring/{id}", tag = "recurring",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Recurring search id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ErrorResponse)))]
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
#[utoipa::path(post, path = "/api/searches/{id}/answer", tag = "searches",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Job id")),
    request_body = AnswerRequest,
    responses(
        (status = 204, description = "Answer stored; the job resumes (ADR-032)"),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 409, description = "Job is not awaiting input", body = ErrorResponse)))]
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

/// Full job detail: the job fields, its results, and the agent journal (ADR-030,
/// empty in workflow mode). `#[serde(flatten)]` keeps the job fields at the top
/// level, so the wire shape is identical to serving `JobView` plus the two
/// arrays — pinned by the contract fixture (ADR-049).
#[derive(Serialize, ToSchema)]
struct JobDetailView {
    #[serde(flatten)]
    job: JobView,
    results: Vec<SearchResult>,
    steps: Vec<AgentStep>,
}

/// The job detail payload, shared by `GET /api/searches/{id}` and the SSE
/// stream (ADR-026) so both surfaces always carry the same shape. Public so the
/// cross-language contract test (ADR-049) can pin its exact wire shape.
pub fn job_detail_json(
    job: &ResearchJob,
    results: &[SearchResult],
    steps: &[AgentStep],
) -> serde_json::Value {
    serde_json::to_value(JobDetailView {
        job: JobView::from(job),
        results: results.to_vec(),
        steps: steps.to_vec(),
    })
    .expect("serializable job detail")
}

/// The recurring-search payload served by `POST`/`GET /api/recurring`. Single
/// serialization path (used by the handlers below) so its shape is pinned once
/// by the contract test (ADR-049).
pub fn recurring_search_json(search: &RecurringSearch) -> serde_json::Value {
    serde_json::to_value(RecurringView::from(search)).expect("serializable recurring view")
}

#[utoipa::path(get, path = "/api/searches/{id}", tag = "searches",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Job id")),
    responses(
        (status = 200, description = "Job detail: fields, results and the agent journal", body = JobDetailView),
        (status = 404, description = "Not found", body = ErrorResponse)))]
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

/// Accumulates one task attempt's API spend (ADR-038).
async fn internal_usage(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(usage): Json<JobUsage>,
) -> Response {
    if let Some(rejection) = check_internal_token(&state, &headers) {
        return rejection;
    }
    match state.ingest.record_usage(job_id, &usage).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(IngestError::JobNotFound) => error_body(StatusCode::NOT_FOUND, "job not found"),
        Err(IngestError::Infrastructure(e)) => {
            tracing::error!(error = %e, "record usage failed");
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

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        assert!(constant_time_eq(b"same-token", b"same-token"));
        assert!(!constant_time_eq(b"same-token", b"diff-token"));
        assert!(!constant_time_eq(b"short", b"longer-token")); // length differs
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
