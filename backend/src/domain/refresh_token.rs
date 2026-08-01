//! Refresh tokens (ADR-008): opaque, single-use, stored hashed for revocation.
//!
//! The plaintext is only ever held by the client (HttpOnly cookie); the
//! database sees its SHA-256 hash, so a leaked table cannot be replayed.
//!
//! Reuse detection (ADR-056): rotation is a *lineage*. Every token belongs to a
//! `family_id` set at login and inherited by each rotation, and a consumed
//! token is kept (marked with `consumed_at`) rather than deleted, so a replay
//! of an already-consumed token is detectable — the sign of a stolen cookie.
//! When that happens the whole family is revoked, killing the thief's rotated
//! token too. Other logins (their own families) stay alive.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct RefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    /// Rotation lineage (ADR-056): one family per login, shared by every token
    /// this login rotates into. Revoked as a unit when reuse is detected.
    pub family_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Set when the token has been rotated away (ADR-056). A consumed token is
    /// retained until it expires so a replay is caught instead of looking like
    /// an unknown token; `None` means still live.
    pub consumed_at: Option<DateTime<Utc>>,
}

impl RefreshToken {
    /// Issues a token starting a **new** family (used at login). Returns the
    /// record to persist and the plaintext to hand to the client (never stored).
    pub fn issue(user_id: Uuid, ttl_days: i64) -> (Self, String) {
        Self::issue_in_family(user_id, Uuid::new_v4(), ttl_days)
    }

    /// Issues the next token of an existing family (used on rotation, ADR-056),
    /// so a whole login lineage shares one `family_id`.
    pub fn issue_in_family(user_id: Uuid, family_id: Uuid, ttl_days: i64) -> (Self, String) {
        // Two v4 UUIDs -> ~244 bits of entropy, hex-encoded.
        let plaintext = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = super::now_utc();
        let record = Self {
            id: Uuid::new_v4(),
            user_id,
            family_id,
            token_hash: Self::hash(&plaintext),
            expires_at: now + Duration::days(ttl_days),
            created_at: now,
            consumed_at: None,
        };
        (record, plaintext)
    }

    /// True once the token has been rotated away (ADR-056): replaying it signals
    /// reuse.
    pub fn is_consumed(&self) -> bool {
        self.consumed_at.is_some()
    }

    pub fn hash(plaintext: &str) -> String {
        Sha256::digest(plaintext.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_returns_a_hashed_record_and_the_plaintext() {
        let user_id = Uuid::new_v4();
        let (record, plaintext) = RefreshToken::issue(user_id, 30);

        assert_eq!(record.user_id, user_id);
        assert_ne!(record.token_hash, plaintext, "plaintext must not be stored");
        assert_eq!(record.token_hash, RefreshToken::hash(&plaintext));
        assert!(!record.is_expired(Utc::now()));
    }

    #[test]
    fn tokens_are_unique() {
        let user_id = Uuid::new_v4();
        let (_, first) = RefreshToken::issue(user_id, 30);
        let (_, second) = RefreshToken::issue(user_id, 30);
        assert_ne!(first, second);
    }

    #[test]
    fn expiry_is_relative_to_ttl() {
        let (record, _) = RefreshToken::issue(Uuid::new_v4(), 30);
        assert!(!record.is_expired(Utc::now() + Duration::days(29)));
        assert!(record.is_expired(Utc::now() + Duration::days(31)));
    }

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(RefreshToken::hash("abc"), RefreshToken::hash("abc"));
        assert_ne!(RefreshToken::hash("abc"), RefreshToken::hash("abd"));
    }

    #[test]
    fn a_fresh_token_is_live_and_starts_a_family() {
        let (record, _) = RefreshToken::issue(Uuid::new_v4(), 30);
        assert!(!record.is_consumed());
        // Each login opens a distinct family.
        let (other, _) = RefreshToken::issue(record.user_id, 30);
        assert_ne!(record.family_id, other.family_id);
    }

    #[test]
    fn rotation_keeps_the_same_family() {
        let (record, _) = RefreshToken::issue(Uuid::new_v4(), 30);
        let (next, _) = RefreshToken::issue_in_family(record.user_id, record.family_id, 30);
        assert_eq!(next.family_id, record.family_id);
        assert_ne!(next.id, record.id);
    }
}
