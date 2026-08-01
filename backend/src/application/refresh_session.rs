//! Refresh-token rotation and revocation (ADR-008).

use std::sync::Arc;

use chrono::Utc;

use crate::application::login_user::SessionTokens;
use crate::domain::ports::{PortError, RefreshTokenRepository, SecurityAudit, TokenService};
use crate::domain::{RefreshToken, SecurityEvent, SecurityEventKind};

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("invalid or expired refresh token")]
    InvalidToken,
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

pub struct RefreshSession {
    refresh_tokens: Arc<dyn RefreshTokenRepository>,
    tokens: Arc<dyn TokenService>,
    audit: Arc<dyn SecurityAudit>,
    refresh_ttl_days: i64,
}

impl RefreshSession {
    pub fn new(
        refresh_tokens: Arc<dyn RefreshTokenRepository>,
        tokens: Arc<dyn TokenService>,
        audit: Arc<dyn SecurityAudit>,
        refresh_ttl_days: i64,
    ) -> Self {
        Self {
            refresh_tokens,
            tokens,
            audit,
            refresh_ttl_days,
        }
    }

    /// Rotation with reuse detection (ADR-056). The presented token is marked
    /// consumed (single use) and the next token of its family is issued.
    ///
    /// Replaying an already-consumed token means the cookie was captured: the
    /// legitimate client and the thief now both hold copies, and whichever
    /// rotates second replays a consumed token. That is treated as a compromise
    /// — the whole family is revoked, killing the thief's rotated token too, and
    /// the user must re-authenticate. Other logins (their own families) survive.
    pub async fn rotate(&self, presented: &str) -> Result<SessionTokens, RefreshError> {
        let stored = self
            .refresh_tokens
            .find_by_hash(&RefreshToken::hash(presented))
            .await?
            .ok_or(RefreshError::InvalidToken)?;

        // Reuse: a consumed token is being replayed -> revoke the whole lineage.
        if stored.is_consumed() {
            self.refresh_tokens.delete_family(stored.family_id).await?;
            // Audit the compromise (ADR-057), best-effort: never let a logging
            // failure change the security outcome (still a 401, family revoked).
            let event = SecurityEvent::new(
                SecurityEventKind::RefreshReuseDetected,
                Some(stored.user_id),
                None,
                format!("family {}", stored.family_id),
            );
            tracing::warn!(
                user_id = %stored.user_id,
                family_id = %stored.family_id,
                "refresh-token reuse detected, revoking the family"
            );
            if let Err(e) = self.audit.record(&event).await {
                tracing::error!(error = %e, "failed to record refresh-reuse event");
            }
            return Err(RefreshError::InvalidToken);
        }

        if stored.is_expired(Utc::now()) {
            // Expired but never used: just garbage-collect it, no family kill.
            self.refresh_tokens.delete(stored.id).await?;
            return Err(RefreshError::InvalidToken);
        }

        // Keep the old token (marked) so a later replay is caught as reuse.
        self.refresh_tokens
            .mark_consumed(stored.id, Utc::now())
            .await?;
        let (record, plaintext) =
            RefreshToken::issue_in_family(stored.user_id, stored.family_id, self.refresh_ttl_days);
        self.refresh_tokens.insert(&record).await?;
        Ok(SessionTokens {
            access_token: self.tokens.issue(stored.user_id)?,
            refresh_token: plaintext,
        })
    }

    /// Logout: revokes the presented token's whole family (ADR-056), so its
    /// consumed ancestors are cleaned up too and the session is fully closed.
    /// Idempotent — an unknown token is already logged out.
    pub async fn revoke(&self, presented: &str) -> Result<(), PortError> {
        if let Some(stored) = self
            .refresh_tokens
            .find_by_hash(&RefreshToken::hash(presented))
            .await?
        {
            self.refresh_tokens.delete_family(stored.family_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::{
        InMemoryRefreshTokenRepository, InMemorySecurityAudit,
    };
    use crate::domain::ports::TokenService;
    use uuid::Uuid;

    struct FakeTokens;
    impl TokenService for FakeTokens {
        fn issue(&self, user_id: Uuid) -> Result<String, PortError> {
            Ok(format!("token-for:{user_id}"))
        }
        fn verify(&self, _token: &str) -> Option<Uuid> {
            None
        }
    }

    fn service() -> (RefreshSession, Arc<InMemoryRefreshTokenRepository>) {
        let (svc, repo, _audit) = service_with_audit();
        (svc, repo)
    }

    #[allow(clippy::type_complexity)]
    fn service_with_audit() -> (
        RefreshSession,
        Arc<InMemoryRefreshTokenRepository>,
        Arc<InMemorySecurityAudit>,
    ) {
        let repo = Arc::new(InMemoryRefreshTokenRepository::default());
        let audit = Arc::new(InMemorySecurityAudit::default());
        (
            RefreshSession::new(repo.clone(), Arc::new(FakeTokens), audit.clone(), 30),
            repo,
            audit,
        )
    }

    async fn seeded_token(repo: &InMemoryRefreshTokenRepository, ttl_days: i64) -> (Uuid, String) {
        let user_id = Uuid::new_v4();
        let (record, plaintext) = RefreshToken::issue(user_id, ttl_days);
        repo.insert(&record).await.unwrap();
        (user_id, plaintext)
    }

    #[tokio::test]
    async fn rotation_issues_a_new_pair_and_the_chain_continues() {
        let (service, repo) = service();
        let (user_id, plaintext) = seeded_token(&repo, 30).await;

        let first = service.rotate(&plaintext).await.unwrap();
        assert_eq!(first.access_token, format!("token-for:{user_id}"));
        assert_ne!(first.refresh_token, plaintext);

        // The freshly issued token rotates again — the lineage continues.
        let second = service.rotate(&first.refresh_token).await.unwrap();
        assert_ne!(second.refresh_token, first.refresh_token);
    }

    #[tokio::test]
    async fn replaying_a_consumed_token_revokes_the_whole_family() {
        // ADR-056: a stolen cookie means the old (consumed) token is replayed.
        // That revokes the entire rotation lineage, current token included.
        let (service, repo, audit) = service_with_audit();
        let (_, original) = seeded_token(&repo, 30).await;

        let first = service.rotate(&original).await.unwrap(); // original now consumed
        let second = service.rotate(&first.refresh_token).await.unwrap(); // current, live

        // Replaying the consumed original is detected as reuse.
        assert!(matches!(
            service.rotate(&original).await.unwrap_err(),
            RefreshError::InvalidToken
        ));

        // ...and the compromise is audited (ADR-057).
        let events = audit.list_recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "refresh_reuse_detected");
        assert!(events[0].user_id.is_some());

        // ...and the whole family is gone: even the current token no longer works.
        assert!(matches!(
            service.rotate(&second.refresh_token).await.unwrap_err(),
            RefreshError::InvalidToken
        ));
        assert!(matches!(
            service.rotate(&first.refresh_token).await.unwrap_err(),
            RefreshError::InvalidToken
        ));
    }

    #[tokio::test]
    async fn reuse_detection_only_revokes_the_offending_family() {
        // A compromise on one login must not log other logins out.
        let (service, repo) = service();
        let (_, device_a) = seeded_token(&repo, 30).await; // login A (its own family)
        let (_, device_b) = seeded_token(&repo, 30).await; // login B (a different family)

        // Rotate A once, then replay its consumed token -> family A revoked.
        let a1 = service.rotate(&device_a).await.unwrap();
        assert!(matches!(
            service.rotate(&device_a).await.unwrap_err(),
            RefreshError::InvalidToken
        ));
        assert!(matches!(
            service.rotate(&a1.refresh_token).await.unwrap_err(),
            RefreshError::InvalidToken
        ));

        // Login B is a different family: still perfectly usable.
        service.rotate(&device_b).await.unwrap();
    }

    #[tokio::test]
    async fn expired_tokens_are_rejected_and_consumed() {
        let (service, repo) = service();
        let (_, plaintext) = seeded_token(&repo, -1).await; // already expired

        let err = service.rotate(&plaintext).await.unwrap_err();
        assert!(matches!(err, RefreshError::InvalidToken));
        assert!(repo
            .find_by_hash(&RefreshToken::hash(&plaintext))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn unknown_tokens_are_rejected() {
        let (service, _) = service();
        let err = service.rotate("never-issued").await.unwrap_err();
        assert!(matches!(err, RefreshError::InvalidToken));
    }

    #[tokio::test]
    async fn revoke_deletes_the_token_and_is_idempotent() {
        let (service, repo) = service();
        let (_, plaintext) = seeded_token(&repo, 30).await;

        service.revoke(&plaintext).await.unwrap();
        assert!(matches!(
            service.rotate(&plaintext).await.unwrap_err(),
            RefreshError::InvalidToken
        ));
        service.revoke(&plaintext).await.unwrap(); // second call is fine
    }
}
