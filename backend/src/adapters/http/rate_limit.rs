//! Fixed-window, per-client-IP rate limiter (ADR-017/037).
//!
//! Two backends behind one middleware:
//! - **in-memory** (default): one process, one HashMap. Good enough for a
//!   single-instance deployment (ADR-015); degradation with N replicas is
//!   benign (effective limit becomes N× the configured one).
//! - **Redis** (opt-in via `RATE_LIMIT_REDIS_URL`, ADR-037): the same fixed
//!   window shared across replicas (`INCR` + `EXPIRE` per window bucket).
//!   **Fail-open**: if Redis is unreachable the request goes through with a
//!   warning — the limiter protects spend, it must not become the outage.
//!
//! The client key is the first `X-Forwarded-For` entry — set by Caddy/nginx in
//! front (trusted, ADR-014/015) — falling back to the socket peer address,
//! then to a shared bucket.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub struct FixedWindowLimiter {
    max_per_window: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl FixedWindowLimiter {
    pub fn per_minute(max: u32) -> Arc<Self> {
        Arc::new(Self {
            max_per_window: max,
            window: Duration::from_secs(60),
            hits: Mutex::new(HashMap::new()),
        })
    }

    /// Records a hit for `key` and tells whether it is still allowed.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    /// Deterministic core, testable without sleeping.
    fn allow_at(&self, key: &str, now: Instant) -> bool {
        let mut hits = self.hits.lock().unwrap();
        // Housekeeping: drop expired windows so the map stays bounded.
        hits.retain(|_, (start, _)| now.duration_since(*start) < self.window);

        let (start, count) = hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(*start) >= self.window {
            (*start, *count) = (now, 0);
        }
        *count += 1;
        *count <= self.max_per_window
    }
}

/// Distributed fixed window (ADR-037): one Redis counter per (scope, client,
/// window index), expiring with the window. The connection is established
/// lazily and multiplexed.
pub struct RedisWindowLimiter {
    client: redis::Client,
    connection: tokio::sync::OnceCell<redis::aio::ConnectionManager>,
    max_per_window: u32,
    window_secs: u64,
    /// Keeps the auth and api limiters on separate counters.
    scope: &'static str,
}

impl RedisWindowLimiter {
    pub fn per_minute(url: &str, max: u32, scope: &'static str) -> Result<Arc<Self>, String> {
        let client = redis::Client::open(url).map_err(|e| format!("invalid redis url: {e}"))?;
        Ok(Arc::new(Self {
            client,
            connection: tokio::sync::OnceCell::new(),
            max_per_window: max,
            window_secs: 60,
            scope,
        }))
    }

    pub async fn allow(&self, key: &str) -> bool {
        match self.try_allow(key).await {
            Ok(allowed) => allowed,
            Err(e) => {
                // Fail-open: rate limiting protects spend; it must not turn a
                // Redis outage into an API outage.
                tracing::warn!(error = %e, "redis rate limiter unavailable, allowing request");
                true
            }
        }
    }

    async fn try_allow(&self, key: &str) -> Result<bool, redis::RedisError> {
        let mut conn = self
            .connection
            .get_or_try_init(|| {
                // Tight budgets: a slow Redis must degrade to fail-open fast,
                // not hold requests hostage.
                let config = redis::aio::ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(Duration::from_secs(1)))
                    .set_response_timeout(Some(Duration::from_secs(1)))
                    .set_number_of_retries(1);
                redis::aio::ConnectionManager::new_with_config(self.client.clone(), config)
            })
            .await?
            .clone();
        let window_index = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / self.window_secs;
        let bucket = format!("rl:{}:{}:{}", self.scope, key, window_index);
        let (count,): (u32,) = redis::pipe()
            .atomic()
            .incr(&bucket, 1)
            .expire(&bucket, self.window_secs as i64 + 1)
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(count <= self.max_per_window)
    }
}

/// The middleware state: in-memory by default, Redis when configured (ADR-037).
#[derive(Clone)]
pub enum Limiter {
    InMemory(Arc<FixedWindowLimiter>),
    Redis(Arc<RedisWindowLimiter>),
}

impl Limiter {
    /// Builds the limiter for a scope: Redis-backed when `redis_url` is set
    /// (falling back to in-memory if the URL cannot even be parsed).
    pub fn per_minute(max: u32, scope: &'static str, redis_url: Option<&str>) -> Self {
        if let Some(url) = redis_url {
            match RedisWindowLimiter::per_minute(url, max, scope) {
                Ok(limiter) => return Limiter::Redis(limiter),
                Err(e) => {
                    tracing::error!(error = %e, "falling back to the in-memory rate limiter");
                }
            }
        }
        Limiter::InMemory(FixedWindowLimiter::per_minute(max))
    }

    pub async fn allow(&self, key: &str) -> bool {
        match self {
            Limiter::InMemory(limiter) => limiter.allow(key),
            Limiter::Redis(limiter) => limiter.allow(key).await,
        }
    }
}

fn client_key(request: &Request) -> String {
    request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|ip| ip.trim().to_string())
        .or_else(|| {
            request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Axum middleware: 429 with a JSON body once the caller exceeds the window.
pub async fn rate_limit(State(limiter): State<Limiter>, request: Request, next: Next) -> Response {
    if !limiter.allow(&client_key(&request)).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "too many requests, slow down" })),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_then_blocks() {
        let limiter = FixedWindowLimiter::per_minute(3);
        let now = Instant::now();
        for _ in 0..3 {
            assert!(limiter.allow_at("1.2.3.4", now));
        }
        assert!(!limiter.allow_at("1.2.3.4", now));
    }

    #[test]
    fn keys_are_independent() {
        let limiter = FixedWindowLimiter::per_minute(1);
        let now = Instant::now();
        assert!(limiter.allow_at("1.2.3.4", now));
        assert!(!limiter.allow_at("1.2.3.4", now));
        assert!(limiter.allow_at("5.6.7.8", now));
    }

    #[test]
    fn window_expiry_resets_the_count() {
        let limiter = FixedWindowLimiter::per_minute(1);
        let start = Instant::now();
        assert!(limiter.allow_at("1.2.3.4", start));
        assert!(!limiter.allow_at("1.2.3.4", start));
        assert!(limiter.allow_at("1.2.3.4", start + Duration::from_secs(61)));
    }
}
