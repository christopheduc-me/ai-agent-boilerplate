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
use backend::adapters::http::rate_limit::Limiter;
use backend::adapters::http::{router_with_limits, AppState, RateLimitConfig};
use backend::adapters::persistence::in_memory::{
    InMemoryJobRepository, InMemoryRecurringSearchRepository, InMemoryRefreshTokenRepository,
    InMemorySecurityAudit, InMemoryUserRepository,
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
        Arc::new(InMemoryRecurringSearchRepository::default()),
        Arc::new(NoopJobDispatcher),
        Arc::new(backend::adapters::digest::NoopDigestSender),
        Arc::new(Argon2PasswordHasher),
        Arc::new(JwtTokenService::new("test-secret", 15)),
        Arc::new(InMemorySecurityAudit::default()),
        Limiter::per_minute(1000, "login", None),
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
async fn openapi_spec_is_served() {
    let app = app();
    let (status, body) = call(
        &app,
        Request::builder()
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["openapi"], "3.1.0");
    assert!(body["paths"]["/api/searches/{id}"]["get"].is_object());
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
    dispatcher.dispatch(&job, &[]).await.unwrap();

    let expected: Value = serde_json::from_str(&fixture("task-request.json")).unwrap();
    assert_eq!(captured.lock().unwrap().as_slice(), &[expected]);
}

#[tokio::test]
async fn backend_consumes_the_question_callback_fixture() {
    let app = app();
    let (bearer, job_id) = user_with_job(&app).await;
    // The question arrives while the job runs (ADR-032).
    call(
        &app,
        post(
            &format!("/internal/jobs/{job_id}/started"),
            String::new(),
            &[("x-internal-token", INTERNAL_TOKEN)],
        ),
    )
    .await;

    let (status, _) = call(
        &app,
        post(
            &format!("/internal/jobs/{job_id}/question"),
            fixture("question-callback.json"),
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
    assert_eq!(detail["status"], "awaiting_input");
    assert!(detail["question"].as_str().unwrap().contains("ambiguous"));
}

#[tokio::test]
async fn backend_produces_the_task_request_with_a_null_clarification() {
    // The fixture is what the dispatcher sends on the FIRST dispatch: the
    // clarification is null until the user answers (ADR-032). The full
    // capture already runs in backend_produces_the_task_request_fixture.
    let fixture: Value = serde_json::from_str(&fixture("task-request.json")).unwrap();
    assert!(fixture["clarification"].is_null());
    assert_eq!(fixture["mode"], "agent");
}

#[tokio::test]
async fn task_request_fixture_carries_the_recurring_memory_field() {
    // First dispatch of a one-shot search: no memory (ADR-033).
    let fixture: Value = serde_json::from_str(&fixture("task-request.json")).unwrap();
    assert_eq!(fixture["seen_urls"], serde_json::json!([]));
}

#[tokio::test]
async fn backend_produces_the_digest_webhook_fixture() {
    // The digest (ADR-036) is consumed by the USER's systems (Slack, n8n,
    // a fork's endpoint…): the fixture pins the outbound shape.
    use backend::domain::ports::{Digest, DigestEntry};
    use chrono::TimeZone;

    let digest = Digest {
        recurring_search_id: Uuid::parse_str("9a1f0c5e-2b7d-4c3a-8e6f-1d2a3b4c5d6e").unwrap(),
        job_id: Uuid::parse_str("3fa85f64-5717-4562-b3fc-2c963f66afa6").unwrap(),
        keyword: "rust releases".into(),
        new_count: 2,
        new_results: vec![
            DigestEntry {
                title: "Rust 1.99 released".into(),
                url: "https://example.com/rust-1-99".into(),
                published_at: Some(chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
            },
            DigestEntry {
                title: "Undated announcement".into(),
                url: "https://example.com/undated".into(),
                published_at: None,
            },
        ],
    };

    let expected: Value = serde_json::from_str(&fixture("digest-webhook.json")).unwrap();
    assert_eq!(serde_json::to_value(&digest).unwrap(), expected);
}

#[tokio::test]
async fn backend_consumes_the_usage_callback_fixture() {
    let app = app();
    let (bearer, job_id) = user_with_job(&app).await;

    // Post it twice: attempts accumulate (ADR-038 — retries spend real money).
    for _ in 0..2 {
        let (status, _) = call(
            &app,
            post(
                &format!("/internal/jobs/{job_id}/usage"),
                fixture("usage-callback.json"),
                &[("x-internal-token", INTERNAL_TOKEN)],
            ),
        )
        .await;
        assert_eq!(status, 204, "the fixture must deserialize as-is");
    }

    let (_, detail) = call(
        &app,
        Request::builder()
            .uri(format!("/api/searches/{job_id}"))
            .header("authorization", &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(detail["usage"]["llm_calls"], 18);
    assert_eq!(detail["usage"]["llm_input_tokens"], 17000);
    assert_eq!(detail["usage"]["search_calls"], 4);
    let cost = detail["usage"]["cost_usd"].as_f64().unwrap();
    assert!((cost - 0.177).abs() < 1e-9, "accumulated cost: {cost}");
}

/// The public API's exact wire shape (ADR-049): the backend PRODUCES the
/// `GET /api/searches/{id}` detail and the `/api/recurring` payload; the
/// fixtures pin them so a Rust rename/field change breaks this suite instead of
/// silently reaching the frontend. The frontend side asserts the same fixtures.
#[tokio::test]
async fn backend_produces_the_public_contract_fixtures() {
    use backend::adapters::http::{job_detail_json, recurring_search_json};
    use backend::domain::{
        AgentStep, DateConfidence, EventType, JobStatus, JobUsage, RecurringSearch, SearchResult,
    };
    use chrono::TimeZone;

    let mut job = ResearchJob::new(Uuid::new_v4(), "rust releases")
        .unwrap()
        .with_mode(JobMode::Agent);
    job.id = Uuid::parse_str("3fa85f64-5717-4562-b3fc-2c963f66afa6").unwrap();
    job.status = JobStatus::Completed;
    job.usage = JobUsage {
        llm_calls: 7,
        llm_input_tokens: 12000,
        llm_output_tokens: 3000,
        search_calls: 2,
        cost_usd: 0.135,
    };
    job.created_at = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 9, 0, 0).unwrap();
    job.completed_at = Some(chrono::Utc.with_ymd_and_hms(2026, 5, 1, 9, 0, 12).unwrap());

    let results = vec![
        SearchResult {
            title: "Rust 1.99 released".into(),
            url: "https://blog.rust-lang.org/rust-1-99".into(),
            snippet: "The Rust team announced 1.99.".into(),
            published_at: Some(chrono::Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
            date_confidence: DateConfidence::High,
            event_type: EventType::Release,
            summary: Some("Rust 1.99 ships faster incremental builds.".into()),
            is_new: true,
            raw: serde_json::json!({"provider": "fixture"}),
        },
        SearchResult {
            title: "An opinion on boring tech".into(),
            url: "https://example.com/boring".into(),
            snippet: "No date stated.".into(),
            published_at: None,
            date_confidence: DateConfidence::Unknown,
            event_type: EventType::Other,
            summary: None,
            is_new: false,
            raw: serde_json::json!({}),
        },
    ];
    let steps = vec![
        AgentStep {
            seq: 1,
            kind: "search".into(),
            detail: "rust 1.99 release".into(),
            reason: "start with the goal".into(),
            new_hits: 2,
        },
        AgentStep {
            seq: 2,
            kind: "finish".into(),
            detail: String::new(),
            reason: "coverage sufficient".into(),
            new_hits: 0,
        },
    ];

    let detail = job_detail_json(&job, &results, &steps);
    let expected: Value = serde_json::from_str(&fixture("search-job-detail.json")).unwrap();
    assert_eq!(
        detail, expected,
        "job detail wire shape drifted from the fixture"
    );

    let recurring = RecurringSearch {
        id: Uuid::parse_str("9a1f0c5e-2b7d-4c3a-8e6f-1d2a3b4c5d6e").unwrap(),
        user_id: Uuid::new_v4(),
        keyword: "rust releases".into(),
        mode: JobMode::Agent,
        interval_minutes: 1440,
        webhook_url: Some("https://example.com/webhook".into()),
        created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 9, 0, 0).unwrap(),
        last_run_at: Some(chrono::Utc.with_ymd_and_hms(2026, 5, 2, 9, 0, 0).unwrap()),
    };
    let expected_recurring: Value =
        serde_json::from_str(&fixture("recurring-search.json")).unwrap();
    assert_eq!(
        recurring_search_json(&recurring),
        expected_recurring,
        "recurring wire shape drifted from the fixture"
    );
}

#[tokio::test]
async fn security_headers_are_set_on_every_response() {
    // ADR-054: hardening headers on all responses, errors included.
    let app = app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let headers = response.headers();
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    assert!(headers
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("default-src 'none'"));
}
