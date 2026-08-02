//! ChannelNotifier adapters (ADR-061): deliver a digest to a user's profile
//! channels — Slack and Telegram today (email planned, ADR-062).
//!
//! Slack posts `{"text": …}` to a user-supplied **incoming-webhook URL**, so it
//! reuses the digest webhook's SSRF guard (ADR-055/056): a filtering DNS
//! resolver refuses non-public hosts, redirects are disabled. Telegram posts to
//! the fixed Bot API host (`api.telegram.org`), so it needs no such guard.
//! Deliveries are best-effort — a dead channel logs and is skipped, exactly like
//! the webhook sender (ADR-036).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::adapters::digest::PublicOnlyResolver;
use crate::domain::ports::{ChannelNotifier, Digest, PortError};
use crate::domain::{ChannelKind, NotificationChannel};

/// Renders a digest as a short plain-text message shared by the text-based
/// channels (Slack markdown tolerates it; Telegram shows it as-is).
fn digest_to_text(digest: &Digest) -> String {
    let mut text = format!(
        "New results for “{}” ({}):",
        digest.keyword, digest.new_count
    );
    for entry in &digest.new_results {
        text.push_str(&format!("\n• {} — {}", entry.title, entry.url));
    }
    text
}

fn short_client(with_resolver: Option<Arc<PublicOnlyResolver>>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(resolver) = with_resolver {
        builder = builder.dns_resolver(resolver);
    }
    builder.build().expect("reqwest client")
}

/// Slack incoming webhook (`{"text": …}`). `allow_private` mirrors the digest
/// sender's opt-in (`DIGEST_ALLOW_PRIVATE_WEBHOOKS`, ADR-055) and skips the SSRF
/// guard — used for an internal Slack-compatible relay, and for tests.
pub struct SlackNotifier {
    client: reqwest::Client,
}

impl SlackNotifier {
    pub fn new(allow_private: bool) -> Self {
        let resolver = (!allow_private).then(|| Arc::new(PublicOnlyResolver));
        Self {
            client: short_client(resolver),
        }
    }
}

#[async_trait]
impl ChannelNotifier for SlackNotifier {
    async fn notify(
        &self,
        channel: &NotificationChannel,
        digest: &Digest,
    ) -> Result<(), PortError> {
        let response = self
            .client
            .post(&channel.target)
            .json(&serde_json::json!({ "text": digest_to_text(digest) }))
            .send()
            .await
            .map_err(|e| PortError(format!("slack unreachable: {e}")))?;
        if !response.status().is_success() {
            return Err(PortError(format!("slack returned {}", response.status())));
        }
        Ok(())
    }
}

/// Telegram Bot API `sendMessage`. `base_url` is `https://api.telegram.org` in
/// production; tests point it at a local stub.
pub struct TelegramNotifier {
    client: reqwest::Client,
    base_url: String,
}

impl TelegramNotifier {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: short_client(None),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl ChannelNotifier for TelegramNotifier {
    async fn notify(
        &self,
        channel: &NotificationChannel,
        digest: &Digest,
    ) -> Result<(), PortError> {
        let token = channel
            .secret
            .as_deref()
            .ok_or_else(|| PortError("telegram channel has no bot token".into()))?;
        let url = format!("{}/bot{}/sendMessage", self.base_url, token);
        let response = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": channel.target,
                "text": digest_to_text(digest),
            }))
            .send()
            .await
            .map_err(|e| PortError(format!("telegram unreachable: {e}")))?;
        if !response.status().is_success() {
            return Err(PortError(format!(
                "telegram returned {}",
                response.status()
            )));
        }
        Ok(())
    }
}

/// Routes a digest to the right sender by channel kind (ADR-061).
pub struct DispatchingChannelNotifier {
    slack: Arc<dyn ChannelNotifier>,
    telegram: Arc<dyn ChannelNotifier>,
}

impl DispatchingChannelNotifier {
    pub fn new(slack: Arc<dyn ChannelNotifier>, telegram: Arc<dyn ChannelNotifier>) -> Self {
        Self { slack, telegram }
    }
}

#[async_trait]
impl ChannelNotifier for DispatchingChannelNotifier {
    async fn notify(
        &self,
        channel: &NotificationChannel,
        digest: &Digest,
    ) -> Result<(), PortError> {
        match channel.kind {
            ChannelKind::Slack => self.slack.notify(channel, digest).await,
            ChannelKind::Telegram => self.telegram.notify(channel, digest).await,
        }
    }
}

/// No-op notifier for tests and wiring points that do not deliver.
#[derive(Default)]
pub struct NoopChannelNotifier;

#[async_trait]
impl ChannelNotifier for NoopChannelNotifier {
    async fn notify(
        &self,
        _channel: &NotificationChannel,
        _digest: &Digest,
    ) -> Result<(), PortError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ports::DigestEntry;
    use axum::extract::State;
    use axum::routing::post;
    use axum::Router;
    use std::sync::Mutex;
    use uuid::Uuid;

    type Received = Arc<Mutex<Vec<(String, String)>>>; // (path, body)

    async fn spawn_stub(status: u16) -> (String, Received) {
        let received: Received = Arc::default();
        let app =
            Router::new()
                .route(
                    "/{*rest}",
                    post(
                        move |State(received): State<Received>,
                              uri: axum::http::Uri,
                              body: String| async move {
                            received
                                .lock()
                                .unwrap()
                                .push((uri.path().to_string(), body));
                            axum::http::StatusCode::from_u16(status).unwrap()
                        },
                    ),
                )
                .with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), received)
    }

    fn a_digest() -> Digest {
        Digest {
            recurring_search_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            keyword: "rust releases".into(),
            new_count: 1,
            new_results: vec![DigestEntry {
                title: "Rust 1.99".into(),
                url: "https://ex.com/rust".into(),
                published_at: None,
            }],
        }
    }

    fn channel(kind: ChannelKind, target: &str, secret: Option<&str>) -> NotificationChannel {
        NotificationChannel {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            kind,
            target: target.into(),
            secret: secret.map(String::from),
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn slack_posts_a_text_payload() {
        let (base, received) = spawn_stub(200).await;
        let ch = channel(ChannelKind::Slack, &format!("{base}/hook"), None);
        // allow_private so the SSRF guard does not block the 127.0.0.1 stub.
        SlackNotifier::new(true)
            .notify(&ch, &a_digest())
            .await
            .unwrap();

        let entries = received.lock().unwrap();
        let (path, body) = &entries[0];
        assert_eq!(path, "/hook");
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(json["text"].as_str().unwrap().contains("rust releases"));
        assert!(json["text"].as_str().unwrap().contains("Rust 1.99"));
    }

    #[tokio::test]
    async fn telegram_calls_send_message_with_the_token_and_chat_id() {
        let (base, received) = spawn_stub(200).await;
        let ch = channel(ChannelKind::Telegram, "chat-42", Some("bot-token"));
        TelegramNotifier::new(base)
            .notify(&ch, &a_digest())
            .await
            .unwrap();

        let entries = received.lock().unwrap();
        let (path, body) = &entries[0];
        assert_eq!(path, "/botbot-token/sendMessage");
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["chat_id"], "chat-42");
        assert!(json["text"].as_str().unwrap().contains("rust releases"));
    }

    #[tokio::test]
    async fn dispatch_routes_by_kind() {
        let (base, received) = spawn_stub(200).await;
        let notifier = DispatchingChannelNotifier::new(
            Arc::new(SlackNotifier::new(true)),
            Arc::new(TelegramNotifier::new(base.clone())),
        );
        notifier
            .notify(
                &channel(ChannelKind::Telegram, "c1", Some("tok")),
                &a_digest(),
            )
            .await
            .unwrap();
        notifier
            .notify(
                &channel(ChannelKind::Slack, &format!("{base}/slack"), None),
                &a_digest(),
            )
            .await
            .unwrap();

        let paths: Vec<String> = received
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert!(paths.contains(&"/bottok/sendMessage".to_string()));
        assert!(paths.contains(&"/slack".to_string()));
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let (base, _) = spawn_stub(500).await;
        let ch = channel(ChannelKind::Slack, &format!("{base}/hook"), None);
        assert!(SlackNotifier::new(true)
            .notify(&ch, &a_digest())
            .await
            .is_err());
    }
}
