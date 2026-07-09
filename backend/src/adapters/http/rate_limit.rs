//! Fixed-window, in-memory, per-client-IP rate limiter (ADR-017).
//!
//! Deliberately simple: one process, one HashMap, a fixed window. Good enough
//! for a single-instance deployment (ADR-015); swap for a Redis-backed limiter
//! when scaling horizontally. The client key is the first `X-Forwarded-For`
//! entry — set by Caddy/nginx in front (trusted, ADR-014/015) — falling back to
//! the socket peer address, then to a shared bucket.

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
pub async fn rate_limit(
    State(limiter): State<Arc<FixedWindowLimiter>>,
    request: Request,
    next: Next,
) -> Response {
    if !limiter.allow(&client_key(&request)) {
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
