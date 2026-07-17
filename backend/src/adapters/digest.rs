//! DigestSender adapters (ADR-036).
//!
//! The webhook adapter POSTs the digest JSON to the URL saved on the
//! recurring search — the universal integration surface (Slack, n8n, Zapier,
//! a fork's own endpoint…). An e-mail sender is one more adapter behind the
//! same port. Deliveries are short-lived and best-effort: the caller treats
//! failures as log-and-continue.

use async_trait::async_trait;

use crate::domain::ports::{Digest, DigestSender, PortError};

pub struct WebhookDigestSender {
    client: reqwest::Client,
}

impl Default for WebhookDigestSender {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        }
    }
}

#[async_trait]
impl DigestSender for WebhookDigestSender {
    async fn send(&self, webhook_url: &str, digest: &Digest) -> Result<(), PortError> {
        let response = self
            .client
            .post(webhook_url)
            .json(digest)
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

    type Received = Arc<Mutex<Vec<serde_json::Value>>>;

    async fn spawn_stub(status: u16) -> (String, Received) {
        let received: Received = Arc::default();
        let app = Router::new()
            .route(
                "/hook",
                post(
                    move |State(received): State<Received>, body: String| async move {
                        received
                            .lock()
                            .unwrap()
                            .push(serde_json::from_str(&body).unwrap());
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
    async fn posts_the_digest_json() {
        let (url, received) = spawn_stub(204).await;
        let digest = a_digest();

        WebhookDigestSender::default()
            .send(&url, &digest)
            .await
            .unwrap();

        let bodies = received.lock().unwrap();
        assert_eq!(bodies[0]["keyword"], "rust releases");
        assert_eq!(bodies[0]["new_count"], 1);
        assert_eq!(bodies[0]["new_results"][0]["title"], "Rust 1.99");
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
