//! Refresh tokens (ADR-008): opaque, single-use, stored hashed for revocation.
//!
//! The plaintext is only ever held by the client (HttpOnly cookie); the
//! database sees its SHA-256 hash, so a leaked table cannot be replayed.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct RefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl RefreshToken {
    /// Issues a new token: returns the record to persist and the plaintext to
    /// hand to the client (never stored).
    pub fn issue(user_id: Uuid, ttl_days: i64) -> (Self, String) {
        // Two v4 UUIDs -> ~244 bits of entropy, hex-encoded.
        let plaintext = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let record = Self {
            id: Uuid::new_v4(),
            user_id,
            token_hash: Self::hash(&plaintext),
            expires_at: now + Duration::days(ttl_days),
            created_at: now,
        };
        (record, plaintext)
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
}
