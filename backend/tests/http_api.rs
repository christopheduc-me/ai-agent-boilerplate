//! End-to-end test of the HTTP API: register -> login -> launch a search ->
//! agent callback with results -> read results sorted by publication date.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use backend::adapters::auth::{Argon2PasswordHasher, JwtTokenService};
use backend::adapters::dispatch::NoopJobDispatcher;
use backend::adapters::http::{router_with_limits, AppState, RateLimitConfig};
use backend::adapters::persistence::in_memory::{
    InMemoryJobRepository, InMemoryRefreshTokenRepository, InMemoryUserRepository,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const INTERNAL_TOKEN: &str = "test-internal-token";

fn app() -> Router {
    app_with(RateLimitConfig::default(), 100)
}

fn app_with(limits: RateLimitConfig, daily_quota: u32) -> Router {
    let state = AppState::new(
        Arc::new(InMemoryUserRepository::default()),
        Arc::new(InMemoryJobRepository::default()),
        Arc::new(InMemoryRefreshTokenRepository::default()),
        Arc::new(NoopJobDispatcher),
        Arc::new(Argon2PasswordHasher),
        Arc::new(JwtTokenService::new("test-secret", 15)),
        INTERNAL_TOKEN.into(),
        daily_quota,
        30,
    );
    router_with_limits(state, limits)
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
