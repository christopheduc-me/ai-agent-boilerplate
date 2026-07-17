//! Integration tests for the Redis-backed rate limiter (ADR-037).
//!
//! They run against the Redis pointed to by `REDIS_URL` — the compose service
//! locally, the CI service in the pipelines. Without `REDIS_URL` they are
//! skipped so `cargo test` stays usable offline. Keys embed a fresh UUID per
//! test, so runs are isolated against a shared Redis.

use backend::adapters::http::rate_limit::Limiter;
use uuid::Uuid;

fn redis_url() -> Option<String> {
    match std::env::var("REDIS_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("skipping redis rate-limit tests: REDIS_URL not set");
            None
        }
    }
}

#[tokio::test]
async fn shared_window_allows_up_to_the_limit_then_blocks() {
    let Some(url) = redis_url() else { return };
    let limiter = Limiter::per_minute(3, "test-auth", Some(&url));
    assert!(
        matches!(limiter, Limiter::Redis(_)),
        "redis backend expected"
    );
    let client = format!("ip-{}", Uuid::new_v4());

    for _ in 0..3 {
        assert!(limiter.allow(&client).await);
    }
    assert!(!limiter.allow(&client).await);

    // A second limiter instance (= another backend replica) shares the state:
    // the same client is still blocked — the whole point of ADR-037.
    let replica = Limiter::per_minute(3, "test-auth", Some(&url));
    assert!(!replica.allow(&client).await);

    // Other clients are unaffected.
    assert!(limiter.allow(&format!("ip-{}", Uuid::new_v4())).await);
}

#[tokio::test]
async fn scopes_have_independent_counters() {
    let Some(url) = redis_url() else { return };
    let client = format!("ip-{}", Uuid::new_v4());
    let auth = Limiter::per_minute(1, "test-scope-a", Some(&url));
    let api = Limiter::per_minute(1, "test-scope-b", Some(&url));

    assert!(auth.allow(&client).await);
    assert!(!auth.allow(&client).await);
    // Exhausting the auth scope does not touch the api scope.
    assert!(api.allow(&client).await);
}

#[tokio::test]
async fn unreachable_redis_fails_open() {
    // Reserved port: connection refused. The limiter must allow requests
    // (availability over strictness) instead of turning into an outage.
    let limiter = Limiter::per_minute(1, "test-down", Some("redis://127.0.0.1:1/"));
    assert!(limiter.allow("ip-1").await);
    assert!(limiter.allow("ip-1").await); // still open, still allowed
}
