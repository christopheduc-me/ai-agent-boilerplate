//! End-to-end test of the HTTP API: register -> login -> launch a search ->
//! agent callback with results -> read results sorted by publication date.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use backend::adapters::auth::{Argon2PasswordHasher, JwtTokenService};
use backend::adapters::dispatch::NoopJobDispatcher;
use backend::adapters::http::rate_limit::Limiter;
use backend::adapters::http::{router_with_limits, AppState, RateLimitConfig};
use backend::adapters::persistence::in_memory::{
    InMemoryJobRepository, InMemoryRecurringSearchRepository, InMemoryRefreshTokenRepository,
    InMemorySecurityAudit, InMemoryUserRepository,
};
use backend::domain::ports::SecurityAudit;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const INTERNAL_TOKEN: &str = "test-internal-token";

fn app() -> Router {
    app_with(RateLimitConfig::default(), 100)
}

fn app_with(limits: RateLimitConfig, daily_quota: u32) -> Router {
    app_with_audit(limits, daily_quota).0
}

/// Same wiring as `app_with`, but also hands back the in-memory audit log so a
/// test can assert what was recorded (ADR-057). The per-account login throttle
/// is built from `limits.login_per_minute`.
fn app_with_audit(
    limits: RateLimitConfig,
    daily_quota: u32,
) -> (Router, Arc<InMemorySecurityAudit>) {
    let audit = Arc::new(InMemorySecurityAudit::default());
    let login_throttle = Limiter::per_minute(limits.login_per_minute, "login", None);
    let state = AppState::new(
        Arc::new(InMemoryUserRepository::default()),
        Arc::new(InMemoryJobRepository::default()),
        Arc::new(InMemoryRefreshTokenRepository::default()),
        Arc::new(InMemoryRecurringSearchRepository::default()),
        Arc::new(NoopJobDispatcher),
        Arc::new(backend::adapters::digest::NoopDigestSender),
        Arc::new(Argon2PasswordHasher),
        Arc::new(JwtTokenService::new("test-secret", 15)),
        audit.clone(),
        login_throttle,
        INTERNAL_TOKEN.into(),
        daily_quota,
        30,
    );
    (router_with_limits(state, limits), audit)
}

/// Extracts the `refresh_token` cookie value from a `set-cookie` response header.
fn refresh_cookie_value(response: &axum::response::Response) -> Option<String> {
    let header = response.headers().get("set-cookie")?.to_str().ok()?;
    assert!(
        header.contains("HttpOnly"),
        "cookie must be HttpOnly: {header}"
    );
    assert!(
        header.contains("SameSite=Strict"),
        "cookie must be SameSite=Strict: {header}"
    );
    header
        .split(';')
        .next()?
        .strip_prefix("refresh_token=")
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn post_json(uri: &str, body: Value, extra_headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn get(uri: &str, extra_headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn full_search_lifecycle() {
    let app = app();

    // Register
    let (status, body) = send(
        &app,
        post_json(
            "/api/auth/register",
            json!({"email": "alice@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "register: {body}");

    // Login
    let (status, body) = send(
        &app,
        post_json(
            "/api/auth/login",
            json!({"email": "alice@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login: {body}");
    let token = body["access_token"].as_str().unwrap().to_string();
    let auth = format!("Bearer {token}");

    // Launch a search
    let (status, body) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "rust hexagonal"}),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "create search: {body}");
    let job_id = body["job_id"].as_str().unwrap().to_string();

    // Worker picked the job up: status becomes running (ADR-016)
    let (status, _) = send(
        &app,
        post_json(
            &format!("/internal/jobs/{job_id}/started"),
            json!({}),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(
        &app,
        get(
            &format!("/api/searches/{job_id}"),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "running");

    // Agent callback: results arrive out of order, backend must sort them
    let (status, _) = send(
        &app,
        post_json(
            &format!("/internal/jobs/{job_id}/results"),
            json!({"results": [
                {"title": "old", "url": "https://a", "snippet": "", "published_at": "2023-01-01T00:00:00Z", "date_confidence": "high"},
                {"title": "no-date", "url": "https://b", "snippet": "", "published_at": null, "date_confidence": "unknown"},
                {"title": "new", "url": "https://c", "snippet": "", "published_at": "2026-06-01T00:00:00Z", "date_confidence": "medium"}
            ]}),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Read back: completed, results newest first, unknown date last
    let (status, body) = send(
        &app,
        get(
            &format!("/api/searches/{job_id}"),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "get search: {body}");
    assert_eq!(body["status"], "completed");
    let titles: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["new", "old", "no-date"]);
}

#[tokio::test]
async fn searches_require_authentication() {
    let app = app();
    let (status, _) = send(
        &app,
        post_json("/api/searches", json!({"keyword": "k"}), &[]),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_token_rotation_and_logout() {
    let app = app();

    // Register + login: the response carries the refresh cookie.
    send(
        &app,
        post_json(
            "/api/auth/register",
            json!({"email": "carol@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/auth/login",
            json!({"email": "carol@example.com", "password": "s3cret-password"}),
            &[],
        ))
        .await
        .unwrap();
    let first_refresh = refresh_cookie_value(&response).expect("login must set the refresh cookie");

    // Refresh rotates: new access token + new cookie.
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/auth/refresh",
            json!({}),
            &[("cookie", &format!("refresh_token={first_refresh}"))],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let second_refresh = refresh_cookie_value(&response).expect("refresh must rotate the cookie");
    assert_ne!(first_refresh, second_refresh);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["access_token"].as_str().is_some());

    // Replaying the consumed token is rejected (single use).
    let (status, _) = send(
        &app,
        post_json(
            "/api/auth/refresh",
            json!({}),
            &[("cookie", &format!("refresh_token={first_refresh}"))],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Logout revokes the current token and clears the cookie.
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/auth/logout",
            json!({}),
            &[("cookie", &format!("refresh_token={second_refresh}"))],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        refresh_cookie_value(&response).is_none(),
        "cookie must be cleared"
    );

    let (status, _) = send(
        &app,
        post_json(
            "/api/auth/refresh",
            json!({}),
            &[("cookie", &format!("refresh_token={second_refresh}"))],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_without_cookie_is_rejected() {
    let app = app();
    let (status, _) = send(&app, post_json("/api/auth/refresh", json!({}), &[])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_endpoints_are_rate_limited_per_ip() {
    let app = app_with(
        RateLimitConfig {
            auth_per_minute: 2,
            api_per_minute: 100,
            login_per_minute: 1000,
            redis_url: None,
        },
        100,
    );
    let attempt = |ip: &'static str| {
        post_json(
            "/api/auth/login",
            json!({"email": "a@b.c", "password": "wrong-password"}),
            &[("x-forwarded-for", ip)],
        )
    };

    for _ in 0..2 {
        let (status, _) = send(&app, attempt("1.2.3.4")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED); // wrong creds, but allowed through
    }
    let (status, body) = send(&app, attempt("1.2.3.4")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");

    // Another client is unaffected.
    let (status, _) = send(&app, attempt("9.9.9.9")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn search_creation_is_capped_by_the_daily_quota() {
    let app = app_with(RateLimitConfig::default(), 1);

    let (_, _) = send(
        &app,
        post_json(
            "/api/auth/register",
            json!({"email": "bob@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let (_, body) = send(
        &app,
        post_json(
            "/api/auth/login",
            json!({"email": "bob@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let auth = format!("Bearer {}", body["access_token"].as_str().unwrap());

    let (status, _) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "first"}),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, body) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "second"}),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["error"].as_str().unwrap().contains("quota"));
}

#[tokio::test]
async fn every_response_carries_a_request_id() {
    let app = app();

    // Generated when absent.
    let response = app.clone().oneshot(get("/healthz", &[])).await.unwrap();
    assert!(response.headers().get("x-request-id").is_some());

    // Echoed when provided (proxy or retrying client sets it).
    let response = app
        .clone()
        .oneshot(get("/healthz", &[("x-request-id", "corr-42")]))
        .await
        .unwrap();
    assert_eq!(response.headers().get("x-request-id").unwrap(), "corr-42");
}

#[tokio::test]
async fn sse_streams_job_updates_until_terminal() {
    use futures_util::StreamExt;

    let app = app();

    // Register + login + launch a job.
    send(
        &app,
        post_json(
            "/api/auth/register",
            json!({"email": "sse@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let (_, body) = send(
        &app,
        post_json(
            "/api/auth/login",
            json!({"email": "sse@example.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let auth = format!("Bearer {}", body["access_token"].as_str().unwrap());
    let (_, body) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "sse"}),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    let job_id = body["job_id"].as_str().unwrap().to_string();

    // Unknown job -> 404, no stream.
    let (status, _) = send(
        &app,
        get(
            &format!("/api/searches/{}/events", uuid::Uuid::new_v4()),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Open the stream: the first frame carries the current (pending) state.
    let response = app
        .clone()
        .oneshot(get(
            &format!("/api/searches/{job_id}/events"),
            &[("authorization", auth.as_str())],
        ))
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let mut frames = response.into_body().into_data_stream();
    let mut received = String::new();

    let first = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
        .await
        .expect("first SSE frame")
        .unwrap()
        .unwrap();
    received.push_str(std::str::from_utf8(&first).unwrap());
    assert!(received.contains("event: update"), "{received}");
    assert!(received.contains(r#""status":"pending""#), "{received}");

    // Worker delivers results: the stream must emit the completed state, then end.
    send(
        &app,
        post_json(
            &format!("/internal/jobs/{job_id}/results"),
            json!({"results": [
                {"title": "r", "url": "https://r", "snippet": "", "published_at": null, "date_confidence": "unknown"}
            ]}),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !received.contains(r#""status":"completed""#) {
        let frame = tokio::time::timeout_at(deadline, frames.next())
            .await
            .expect("completed update before timeout")
            .expect("stream ended before the completed update")
            .unwrap();
        received.push_str(std::str::from_utf8(&frame).unwrap());
    }

    // Terminal state emitted -> the stream closes.
    let end = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
        .await
        .expect("stream should close after the terminal update");
    assert!(end.is_none(), "stream must end after a terminal status");
}

#[tokio::test]
async fn internal_endpoints_require_the_internal_token() {
    let app = app();
    let (status, _) = send(
        &app,
        post_json(
            &format!("/internal/jobs/{}/results", uuid::Uuid::new_v4()),
            json!({"results": []}),
            &[("x-internal-token", "wrong-token")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Agent mode (ADR-030): mode round-trips, the journal is recorded through the
/// internal endpoint (idempotently) and served in the detail payload.
#[tokio::test]
async fn agent_mode_lifecycle_with_journal() {
    let app = app();
    let creds = json!({"email": "agent@example.com", "password": "s3cret-password"});
    send(&app, post_json("/api/auth/register", creds.clone(), &[])).await;
    let (_, login) = send(&app, post_json("/api/auth/login", creds, &[])).await;
    let auth = format!("Bearer {}", login["access_token"].as_str().unwrap());

    let (status, body) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "rust", "mode": "agent"}),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "create agent search: {body}");
    let job_id = body["job_id"].as_str().unwrap().to_string();

    let step = |seq: i32, kind: &str| json!({"seq": seq, "kind": kind, "detail": "rust", "reason": "because", "new_hits": 2});
    for payload in [step(1, "search"), step(1, "search"), step(2, "finish")] {
        let (status, _) = send(
            &app,
            post_json(
                &format!("/internal/jobs/{job_id}/steps"),
                payload,
                &[("x-internal-token", INTERNAL_TOKEN)],
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    let (_, detail) = send(
        &app,
        get(
            &format!("/api/searches/{job_id}"),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(detail["mode"], "agent");
    let steps = detail["steps"].as_array().unwrap();
    // The duplicated seq 1 (Celery retry) was ignored: idempotence (ADR-016).
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["kind"], "search");
    assert_eq!(steps[1]["kind"], "finish");

    // A workflow search keeps the default mode and an empty journal.
    let (_, body) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "rust"}),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    let workflow_id = body["job_id"].as_str().unwrap().to_string();
    let (_, detail) = send(
        &app,
        get(
            &format!("/api/searches/{workflow_id}"),
            &[("authorization", auth.as_str())],
        ),
    )
    .await;
    assert_eq!(detail["mode"], "workflow");
    assert_eq!(detail["steps"].as_array().unwrap().len(), 0);
}

/// Human-in-the-loop lifecycle (ADR-032): the agent asks, the job pauses, the
/// user answers, the job is re-dispatched with a fresh journal.
#[tokio::test]
async fn clarification_lifecycle() {
    let app = app();
    let creds = json!({"email": "hitl@test.dev", "password": "s3cret-password"});
    send(&app, post_json("/api/auth/register", creds.clone(), &[])).await;
    let (_, login) = send(&app, post_json("/api/auth/login", creds, &[])).await;
    let bearer = format!("Bearer {}", login["access_token"].as_str().unwrap());

    let (_, launched) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "jaguar", "mode": "agent"}),
            &[("authorization", &bearer)],
        ),
    )
    .await;
    let job_id = launched["job_id"].as_str().unwrap().to_string();

    // Worker starts, records a step, then asks for clarification.
    let internal = [("x-internal-token", INTERNAL_TOKEN)];
    send(
        &app,
        post_json(
            &format!("/internal/jobs/{job_id}/started"),
            json!({}),
            &internal,
        ),
    )
    .await;
    send(
        &app,
        post_json(
            &format!("/internal/jobs/{job_id}/steps"),
            json!({"seq": 1, "kind": "search", "detail": "jaguar", "reason": "r", "new_hits": 2}),
            &internal,
        ),
    )
    .await;
    let (status, _) = send(
        &app,
        post_json(
            &format!("/internal/jobs/{job_id}/question"),
            json!({"question": "The animal or the car?"}),
            &internal,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, detail) = send(
        &app,
        get(
            &format!("/api/searches/{job_id}"),
            &[("authorization", &bearer)],
        ),
    )
    .await;
    assert_eq!(detail["status"], "awaiting_input");
    assert_eq!(detail["question"], "The animal or the car?");

    // Answering an already-answered / non-awaiting job later conflicts; the
    // happy path requeues and clears the journal (replace semantics).
    let (status, _) = send(
        &app,
        post_json(
            &format!("/api/searches/{job_id}/answer"),
            json!({"answer": "the car"}),
            &[("authorization", &bearer)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, detail) = send(
        &app,
        get(
            &format!("/api/searches/{job_id}"),
            &[("authorization", &bearer)],
        ),
    )
    .await;
    assert_eq!(detail["status"], "pending");
    assert_eq!(detail["answer"], "the car");
    assert_eq!(detail["question"], "The animal or the car?");
    assert!(detail["steps"].as_array().unwrap().is_empty());

    let (status, _) = send(
        &app,
        post_json(
            &format!("/api/searches/{job_id}/answer"),
            json!({"answer": "again"}),
            &[("authorization", &bearer)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// Recurring searches CRUD (ADR-033) over the public API.
#[tokio::test]
async fn recurring_search_crud() {
    let app = app();
    let creds = json!({"email": "recurring@test.dev", "password": "s3cret-password"});
    send(&app, post_json("/api/auth/register", creds.clone(), &[])).await;
    let (_, login) = send(&app, post_json("/api/auth/login", creds, &[])).await;
    let bearer = format!("Bearer {}", login["access_token"].as_str().unwrap());
    let auth = [("authorization", bearer.as_str())];

    let (status, created) = send(
        &app,
        post_json(
            "/api/recurring",
            json!({"keyword": "rust releases", "mode": "agent", "interval_minutes": 60}),
            &auth,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["mode"], "agent");
    assert!(created["last_run_at"].is_null());
    let id = created["id"].as_str().unwrap().to_string();

    let (_, listed) = send(&app, get("/api/recurring", &auth)).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // Interval validation.
    let (status, _) = send(
        &app,
        post_json(
            "/api/recurring",
            json!({"keyword": "x", "interval_minutes": 0}),
            &auth,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let delete = |uri: String| {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .header("authorization", &bearer)
            .body(Body::empty())
            .unwrap()
    };
    let (status, _) = send(&app, delete(format!("/api/recurring/{id}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, delete(format!("/api/recurring/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, listed) = send(&app, get("/api/recurring", &auth)).await;
    assert!(listed.as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------- security audit + per-account throttle (ADR-057)

#[tokio::test]
async fn login_is_throttled_per_account_and_audited() {
    // Cap login at 2 attempts/account/window; keep the per-IP limiter generous
    // so we exercise the per-account throttle, not the IP one.
    let (app, audit) = app_with_audit(
        RateLimitConfig {
            auth_per_minute: 100,
            api_per_minute: 100,
            login_per_minute: 2,
            redis_url: None,
        },
        100,
    );
    send(
        &app,
        post_json(
            "/api/auth/register",
            json!({"email": "victim@b.com", "password": "correct-horse"}),
            &[],
        ),
    )
    .await;

    let bad = json!({"email": "victim@b.com", "password": "wrong"});
    let ip = &[("x-forwarded-for", "9.9.9.9")];
    // Two failed attempts stay under the cap: 401 each.
    for _ in 0..2 {
        let (status, _) = send(&app, post_json("/api/auth/login", bad.clone(), ip)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // Third attempt is throttled regardless of the password.
    let (status, body) = send(&app, post_json("/api/auth/login", bad.clone(), ip)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    // Even the correct password is refused while the account is throttled.
    let good = json!({"email": "victim@b.com", "password": "correct-horse"});
    let (status, _) = send(&app, post_json("/api/auth/login", good, ip)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // The audit log recorded the failures and the throttling, with the IP.
    let events = audit.list_recent(10).await.unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"login_failed"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"login_throttled"), "kinds: {kinds:?}");
    assert!(events
        .iter()
        .any(|e| e.client_ip.as_deref() == Some("9.9.9.9")));
}

#[tokio::test]
async fn exceeding_the_daily_quota_is_audited() {
    let (app, audit) = app_with_audit(RateLimitConfig::default(), 0); // quota 0: first search denied
    send(
        &app,
        post_json(
            "/api/auth/register",
            json!({"email": "q@b.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let (_, login) = send(
        &app,
        post_json(
            "/api/auth/login",
            json!({"email": "q@b.com", "password": "s3cret-password"}),
            &[],
        ),
    )
    .await;
    let auth = format!("Bearer {}", login["access_token"].as_str().unwrap());

    let (status, _) = send(
        &app,
        post_json(
            "/api/searches",
            json!({"keyword": "rust"}),
            &[
                ("authorization", auth.as_str()),
                ("x-forwarded-for", "7.7.7.7"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    let events = audit.list_recent(10).await.unwrap();
    assert!(events
        .iter()
        .any(|e| e.kind == "quota_exceeded" && e.user_id.is_some()));
}
