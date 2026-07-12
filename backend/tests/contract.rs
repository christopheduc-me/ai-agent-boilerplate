//! Cross-language contract fixtures (ADR-025): the backend side.
//!
//! The backend CONSUMES the callback bodies produced by the Python agent —
//! asserted by posting the fixtures through the real router — and PRODUCES
//! the task request — asserted by capturing what the dispatcher sends.

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use backend::adapters::auth::{Argon2PasswordHasher, JwtTokenService};
use backend::adapters::dispatch::{HttpJobDispatcher, NoopJobDispatcher};
use backend::adapters::http::{router_with_limits, AppState, RateLimitConfig};
use backend::adapters::persistence::in_memory::{
    InMemoryJobRepository, InMemoryRefreshTokenRepository, InMemoryUserRepository,
};
use backend::domain::ports::JobDispatcher;
use backend::domain::{JobMode, ResearchJob};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const INTERNAL_TOKEN: &str = "test-internal-token";

fn fixture(name: &str) -> String {
    let path = format!("{}/../contracts/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

fn app() -> Router {
    let state = AppState::new(
        Arc::new(InMemoryUserRepository::default()),
        Arc::new(InMemoryJobRepository::default()),
        Arc::new(InMemoryRefreshTokenRepository::default()),
        Arc::new(NoopJobDispatcher),
        Arc::new(Argon2PasswordHasher),
        Arc::new(JwtTokenService::new("test-secret", 15)),
        INTERNAL_TOKEN.into(),
        100,
        30,
    );
    router_with_limits(state, RateLimitConfig::default())
}

async fn call(app: &Router, request: Request<Body>) -> (u16, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn post(uri: &str, body: String, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body)).unwrap()
}

/// Registers + logs in + launches a search; returns (bearer, job_id).
async fn user_with_job(app: &Router) -> (String, String) {
    let creds = r#"{"email":"contract@test.dev","password":"s3cret-password"}"#;
    call(app, post("/api/auth/register", creds.into(), &[])).await;
    let (_, login) = call(app, post("/api/auth/login", creds.into(), &[])).await;
    let bearer = format!("Bearer {}", login["access_token"].as_str().unwrap());
    let (_, launched) = call(
        app,
        post(
            "/api/searches",
            r#"{"keyword":"contract"}"#.into(),
            &[("authorization", &bearer)],
        ),
    )
    .await;
    (bearer, launched["job_id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn backend_consumes_the_results_callback_fixture() {
    let app = app();
    let (bearer, job_id) = user_with_job(&app).await;

    let (status, _) = call(
        &app,
        post(
            &format!("/internal/jobs/{job_id}/results"),
            fixture("results-callback.json"),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;
    assert_eq!(status, 204, "the fixture must deserialize as-is");

    let (_, detail) = call(
        &app,
        Request::builder()
            .uri(format!("/api/searches/{job_id}"))
            .header("authorization", &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(detail["status"], "completed");
    let results = detail["results"].as_array().unwrap();
    let titles: Vec<&str> = results
        .iter()
        .map(|r| r["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["provider-dated", "llm-dated", "undated"]);
    let confidences: Vec<&str> = results
        .iter()
        .map(|r| r["date_confidence"].as_str().unwrap())
        .collect();
    assert_eq!(confidences, vec!["high", "medium", "unknown"]);
    assert!(results[2]["published_at"].is_null());
}

#[tokio::test]
async fn backend_consumes_the_failure_callback_fixture() {
    let app = app();
    let (bearer, job_id) = user_with_job(&app).await;

    let (status, _) = call(
        &app,
        post(
            &format!("/internal/jobs/{job_id}/failure"),
            fixture("failure-callback.json"),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;
    assert_eq!(status, 204);

    let (_, detail) = call(
        &app,
        Request::builder()
            .uri(format!("/api/searches/{job_id}"))
            .header("authorization", &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(detail["status"], "failed");
    assert!(detail["error"].as_str().unwrap().contains("TAVILY_API_KEY"));
}

#[tokio::test]
async fn backend_consumes_the_agent_step_callback_fixture() {
    let app = app();
    let (bearer, job_id) = user_with_job(&app).await;

    let (status, _) = call(
        &app,
        post(
            &format!("/internal/jobs/{job_id}/steps"),
            fixture("agent-step-callback.json"),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;
    assert_eq!(status, 204, "the fixture must deserialize as-is");

    let (_, detail) = call(
        &app,
        Request::builder()
            .uri(format!("/api/searches/{job_id}"))
            .header("authorization", &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let steps = detail["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["kind"], "search");
    assert_eq!(steps[0]["new_hits"], 4);
}

#[tokio::test]
async fn backend_produces_the_task_request_fixture() {
    // Stub agent API capturing the request body.
    let captured: Arc<std::sync::Mutex<Vec<Value>>> = Arc::default();
    let stub = {
        let captured = captured.clone();
        Router::new().route(
            "/tasks",
            axum::routing::post(move |body: String| {
                let captured = captured.clone();
                async move {
                    captured
                        .lock()
                        .unwrap()
                        .push(serde_json::from_str(&body).unwrap());
                    "queued"
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

    let dispatcher = HttpJobDispatcher::new(format!("http://{addr}"), "secret".into());
    let mut job = ResearchJob::new(Uuid::new_v4(), "rust hexagonal architecture")
        .unwrap()
        .with_mode(JobMode::Agent);
    job.id = Uuid::parse_str("3fa85f64-5717-4562-b3fc-2c963f66afa6").unwrap();
    dispatcher.dispatch(&job).await.unwrap();

    let expected: Value = serde_json::from_str(&fixture("task-request.json")).unwrap();
    assert_eq!(captured.lock().unwrap().as_slice(), &[expected]);
}
