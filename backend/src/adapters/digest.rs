//! DigestSender adapters (ADR-036).
//!
//! The webhook adapter POSTs the digest JSON to the URL saved on the
//! recurring search — the universal integration surface (Slack, n8n, Zapier,
//! a fork's own endpoint…). An e-mail sender is one more adapter behind the
//! same port. Deliveries are short-lived and best-effort: the caller treats
//! failures as log-and-continue.
//!
//! Digests are **at-least-once** (a redelivered completion re-sends the same
//! `job_id`, so consumers dedup on it). When a signing secret is configured
//! (ADR-047) each POST also carries an HMAC-SHA256 of the body in the
//! `X-Signature-256` header, so the consumer can *authenticate* it — a public
//! webhook URL is otherwise forgeable by anyone who learns it.

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::domain::ports::{Digest, DigestSender, PortError};

type HmacSha256 = Hmac<Sha256>;

/// Header carrying `sha256=<hex HMAC of the raw body>` (GitHub convention).
const SIGNATURE_HEADER: &str = "X-Signature-256";

pub struct WebhookDigestSender {
    client: reqwest::Client,
    secret: Option<String>,
}

impl WebhookDigestSender {
    /// With a `secret`, every digest is signed with HMAC-SHA256 (ADR-047);
    /// an empty secret is treated as no secret (unsigned).
    pub fn new(secret: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            secret: secret.filter(|s| !s.is_empty()),
        }
    }
}

impl Default for WebhookDigestSender {
    fn default() -> Self {
        Self::new(None)
    }
}

/// `sha256=<hex>` of `HMAC-SHA256(secret, body)` — what the consumer recomputes
/// over the raw bytes it received and compares (in constant time) to the header.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}

#[async_trait]
impl DigestSender for WebhookDigestSender {
    async fn send(&self, webhook_url: &str, digest: &Digest) -> Result<(), PortError> {
        // Serialize once so the signature covers exactly the bytes we send.
        let body = serde_json::to_vec(digest)
            .map_err(|e| PortError(format!("digest serialization failed: {e}")))?;
        let mut request = self
            .client
            .post(webhook_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        if let Some(secret) = &self.secret {
            request = request.header(SIGNATURE_HEADER, sign(secret, &body));
        }
        let response = request
            .send()
            .await
            .map_err(|e| PortError(format!("webhook unreachable: {e}")))?;
        if !response.status().is_success() {
            return Err(PortError(format!("webhook returned {}", response.status())));
        }
        Ok(())
    }
}

/// No-op sender for tests and for wiring points that do not deliver digests.
#[derive(Default)]
pub struct NoopDigestSender;

#[async_trait]
impl DigestSender for NoopDigestSender {
    async fn send(&self, _webhook_url: &str, _digest: &Digest) -> Result<(), PortError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    /// Captures each request's signature header (if any) and its raw body.
    type Received = Arc<Mutex<Vec<(Option<String>, String)>>>;

    async fn spawn_stub(status: u16) -> (String, Received) {
        let received: Received = Arc::default();
        let app = Router::new()
            .route(
                "/hook",
                post(
                    move |State(received): State<Received>,
                          headers: axum::http::HeaderMap,
                          body: String| async move {
                        let sig = headers
                            .get(SIGNATURE_HEADER)
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        received.lock().unwrap().push((sig, body));
                        axum::http::StatusCode::from_u16(status).unwrap()
                    },
                ),
            )
            .with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/hook"), received)
    }

    fn a_digest() -> Digest {
        Digest {
            recurring_search_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            keyword: "rust releases".into(),
            new_count: 1,
            new_results: vec![crate::domain::ports::DigestEntry {
                title: "Rust 1.99".into(),
                url: "https://ex.com/rust".into(),
                published_at: None,
            }],
        }
    }

    #[tokio::test]
    async fn posts_the_digest_json_unsigned_by_default() {
        let (url, received) = spawn_stub(204).await;

        WebhookDigestSender::default()
            .send(&url, &a_digest())
            .await
            .unwrap();

        let entries = received.lock().unwrap();
        let (sig, raw) = &entries[0];
        assert!(sig.is_none(), "no signature without a secret");
        let body: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(body["keyword"], "rust releases");
        assert_eq!(body["new_results"][0]["title"], "Rust 1.99");
    }

    #[tokio::test]
    async fn signs_the_body_with_hmac_sha256_when_a_secret_is_set() {
        let (url, received) = spawn_stub(204).await;

        WebhookDigestSender::new(Some("s3cret".into()))
            .send(&url, &a_digest())
            .await
            .unwrap();

        let entries = received.lock().unwrap();
        let (sig, raw) = &entries[0];
        let sig = sig.as_deref().expect("signature header present");
        // Exactly the consumer's check: recompute over the received bytes.
        assert_eq!(sig, sign("s3cret", raw.as_bytes()));
        assert!(sig.starts_with("sha256="));
    }

    #[test]
    fn sign_matches_a_known_hmac_sha256_vector() {
        // RFC-style vector — guards the algorithm itself, not just self-consistency.
        assert_eq!(
            sign("key", b"The quick brown fox jumps over the lazy dog"),
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8",
        );
    }

    #[tokio::test]
    async fn non_success_statuses_surface_as_errors() {
        let (url, _) = spawn_stub(500).await;
        let err = WebhookDigestSender::default()
            .send(&url, &a_digest())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("500"));
    }
}
