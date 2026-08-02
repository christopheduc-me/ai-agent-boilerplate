//! Per-user notification channels (ADR-061): where a user receives digests,
//! chosen in their profile — in addition to the optional per-recurring-search
//! webhook (ADR-036, kept for backward compatibility).
//!
//! A channel is a `kind` plus a `target` and an optional `secret`:
//!   * Slack    — `target` is an incoming-webhook URL, no secret.
//!   * Telegram — `target` is the chat id, `secret` is the bot token.
//!   * Email (ADR-062) — `target` is the address, no secret (SMTP is server-level).
//!
//! Pure domain: delivery is a port (`ChannelNotifier`), storage another
//! (`NotificationChannelRepository`).

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Length caps (ADR-056 family): user-supplied free text, bounded before storage.
pub const MAX_TARGET_LEN: usize = 2_048;
pub const MAX_SECRET_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Slack,
    Telegram,
    Email,
}

impl ChannelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Telegram => "telegram",
            Self::Email => "email",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "slack" => Some(Self::Slack),
            "telegram" => Some(Self::Telegram),
            "email" => Some(Self::Email),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ChannelError {
    #[error("unknown channel kind")]
    UnknownKind,
    #[error("target must not be empty")]
    EmptyTarget,
    #[error("target is too long")]
    TargetTooLong,
    #[error("secret is too long")]
    SecretTooLong,
    #[error("slack target must be an https:// incoming webhook URL")]
    InvalidSlackUrl,
    #[error("telegram requires a bot token")]
    MissingTelegramToken,
    #[error("email target must be a valid address")]
    InvalidEmail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationChannel {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: ChannelKind,
    pub target: String,
    /// Sensitive per-channel secret (Telegram bot token). Stored like the
    /// digest signing secret (ADR-047): plaintext, it is the user's own
    /// integration credential — never exposed back in API responses.
    pub secret: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl NotificationChannel {
    pub fn new(
        user_id: Uuid,
        kind: ChannelKind,
        target: &str,
        secret: Option<&str>,
    ) -> Result<Self, ChannelError> {
        let target = target.trim();
        if target.is_empty() {
            return Err(ChannelError::EmptyTarget);
        }
        if target.chars().count() > MAX_TARGET_LEN {
            return Err(ChannelError::TargetTooLong);
        }
        let secret = secret.map(str::trim).filter(|s| !s.is_empty());
        if let Some(s) = secret {
            if s.chars().count() > MAX_SECRET_LEN {
                return Err(ChannelError::SecretTooLong);
            }
        }
        match kind {
            ChannelKind::Slack => {
                if !target.starts_with("https://") {
                    return Err(ChannelError::InvalidSlackUrl);
                }
            }
            ChannelKind::Telegram => {
                if secret.is_none() {
                    return Err(ChannelError::MissingTelegramToken);
                }
            }
            ChannelKind::Email => {
                // A pragmatic sanity check (not full RFC 5322): one `@` with a
                // non-empty local part and a dotted domain. SMTP credentials are
                // server-level config, so an email channel carries no secret.
                let valid = target.split_once('@').is_some_and(|(local, domain)| {
                    !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
                });
                if !valid {
                    return Err(ChannelError::InvalidEmail);
                }
            }
        }
        Ok(Self {
            id: Uuid::new_v4(),
            user_id,
            kind,
            target: target.to_string(),
            secret: secret.map(str::to_string),
            created_at: super::now_utc(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_requires_an_https_target() {
        let user = Uuid::new_v4();
        let ok =
            NotificationChannel::new(user, ChannelKind::Slack, "https://hooks.slack.com/x", None)
                .unwrap();
        assert_eq!(ok.kind, ChannelKind::Slack);
        assert!(ok.secret.is_none());
        assert_eq!(
            NotificationChannel::new(user, ChannelKind::Slack, "http://insecure", None)
                .unwrap_err(),
            ChannelError::InvalidSlackUrl
        );
    }

    #[test]
    fn telegram_requires_a_token() {
        let user = Uuid::new_v4();
        let ok = NotificationChannel::new(user, ChannelKind::Telegram, "12345", Some("bot:tok"))
            .unwrap();
        assert_eq!(ok.target, "12345");
        assert_eq!(ok.secret.as_deref(), Some("bot:tok"));
        assert_eq!(
            NotificationChannel::new(user, ChannelKind::Telegram, "12345", None).unwrap_err(),
            ChannelError::MissingTelegramToken
        );
    }

    #[test]
    fn empty_and_overlong_inputs_are_rejected() {
        let user = Uuid::new_v4();
        assert_eq!(
            NotificationChannel::new(user, ChannelKind::Slack, "   ", None).unwrap_err(),
            ChannelError::EmptyTarget
        );
        let long = format!("https://{}", "a".repeat(MAX_TARGET_LEN));
        assert_eq!(
            NotificationChannel::new(user, ChannelKind::Slack, &long, None).unwrap_err(),
            ChannelError::TargetTooLong
        );
    }

    #[test]
    fn email_requires_a_valid_address() {
        let user = Uuid::new_v4();
        let ok =
            NotificationChannel::new(user, ChannelKind::Email, "me@example.com", None).unwrap();
        assert_eq!(ok.target, "me@example.com");
        assert!(ok.secret.is_none());
        for bad in ["not-an-email", "@example.com", "me@nodot", "me@.com"] {
            assert_eq!(
                NotificationChannel::new(user, ChannelKind::Email, bad, None).unwrap_err(),
                ChannelError::InvalidEmail,
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn kind_strings_roundtrip() {
        for kind in [
            ChannelKind::Slack,
            ChannelKind::Telegram,
            ChannelKind::Email,
        ] {
            assert_eq!(ChannelKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ChannelKind::parse("carrier-pigeon"), None);
    }
}
