//! Security audit events (ADR-057): an append-only record of abuse-relevant
//! moments — failed/throttled logins, refresh-token reuse (ADR-056), quota
//! hits — so an operator can spot credential-stuffing or a stolen cookie after
//! the fact. Pure domain: the storage is a port (`SecurityAudit`), the emission
//! points live in the use cases and HTTP handlers, and every event is also
//! logged (ADR-018) so it surfaces without a database query.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The kind of event. Serialized to a stable string so a fork can add its own
/// kinds without a schema change (the column is free text, like `AgentStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityEventKind {
    /// Wrong password or unknown email on `/login`.
    LoginFailed,
    /// A login attempt refused by the per-account throttle (ADR-057).
    LoginThrottled,
    /// A consumed refresh token was replayed — reuse detection (ADR-056).
    RefreshReuseDetected,
    /// A user hit the daily search quota (ADR-017).
    QuotaExceeded,
}

impl SecurityEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LoginFailed => "login_failed",
            Self::LoginThrottled => "login_throttled",
            Self::RefreshReuseDetected => "refresh_reuse_detected",
            Self::QuotaExceeded => "quota_exceeded",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SecurityEvent {
    pub id: Uuid,
    /// Stable machine string of the kind (`SecurityEventKind::as_str`).
    pub kind: String,
    /// The account concerned, when known (unknown-email logins carry `None`).
    pub user_id: Option<Uuid>,
    /// Client IP when the event originates at the HTTP edge; `None` for events
    /// detected deeper in a use case (e.g. refresh reuse).
    pub client_ip: Option<String>,
    /// Short human context for triage (e.g. the attempted email, the keyword).
    pub detail: String,
    pub created_at: DateTime<Utc>,
}

impl SecurityEvent {
    pub fn new(
        kind: SecurityEventKind,
        user_id: Option<Uuid>,
        client_ip: Option<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: kind.as_str().to_string(),
            user_id,
            client_ip,
            detail: detail.into(),
            created_at: super::now_utc(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stamps_the_kind_string_and_an_id() {
        let event = SecurityEvent::new(
            SecurityEventKind::LoginFailed,
            None,
            Some("1.2.3.4".into()),
            "a@b.com",
        );
        assert_eq!(event.kind, "login_failed");
        assert_eq!(event.detail, "a@b.com");
        assert!(event.user_id.is_none());
        assert!(!event.id.is_nil());
    }

    #[test]
    fn kind_strings_are_stable_and_distinct() {
        let kinds = [
            SecurityEventKind::LoginFailed,
            SecurityEventKind::LoginThrottled,
            SecurityEventKind::RefreshReuseDetected,
            SecurityEventKind::QuotaExceeded,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in kinds {
            assert!(seen.insert(k.as_str()), "kind strings must be unique");
        }
    }
}
