//! Security headers on every response (ADR-054).
//!
//! The backend serves a JSON API, so the policy is maximally strict: a browser
//! should never load anything from, or frame, an API endpoint. `Strict-Transport-
//! Security` is deliberately *not* set here — HSTS belongs at the TLS edge (the
//! Caddy reverse proxy), since the app sits behind it over plain HTTP. The SPA's
//! own (looser) CSP is set by nginx on the HTML responses.

use axum::extract::Request;
use axum::http::header::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

/// Adds the standard hardening headers to every response, including error
/// responses (it wraps the whole router as the outermost layer).
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    response
}
