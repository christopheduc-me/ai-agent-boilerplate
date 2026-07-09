//! Refresh-token rotation and revocation (ADR-008).

use std::sync::Arc;

use chrono::Utc;

use crate::application::login_user::SessionTokens;
use crate::domain::ports::{PortError, RefreshTokenRepository, TokenService};
use crate::domain::RefreshToken;

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
    refresh_ttl_days: i64,
}

impl RefreshSession {
    pub fn new(
        refresh_tokens: Arc<dyn RefreshTokenRepository>,
        tokens: Arc<dyn TokenService>,
        refresh_ttl_days: i64,
    ) -> Self {
        Self {
            refresh_tokens,
            tokens,
            refresh_ttl_days,
        }
    }

    /// Rotation: the presented token is consumed (single use) and a fresh pair
    /// is issued. A replayed token therefore fails — the session was either
    /// legitimately rotated or stolen; both warrant re-authentication.
    pub async fn rotate(&self, presented: &str) -> Result<SessionTokens, RefreshError> {
        let stored = self
            .refresh_tokens
            .find_by_hash(&RefreshToken::hash(presented))
            .await?
            .ok_or(RefreshError::InvalidToken)?;

        // Consume it no matter what: expired tokens are garbage-collected on use.
        self.refresh_tokens.delete(stored.id).await?;
        if stored.is_expired(Utc::now()) {
            return Err(RefreshError::InvalidToken);
        }

        let (record, plaintext) = RefreshToken::issue(stored.user_id, self.refresh_ttl_days);
        self.refresh_tokens.insert(&record).await?;
        Ok(SessionTokens {
            access_token: self.tokens.issue(stored.user_id)?,
            refresh_token: plaintext,
        })
    }

    /// Logout: revokes the presented token. Idempotent — an unknown token is
    /// already logged out.
    pub async fn revoke(&self, presented: &str) -> Result<(), PortError> {
        if let Some(stored) = self
            .refresh_tokens
            .find_by_hash(&RefreshToken::hash(presented))
            .await?
        {
            self.refresh_tokens.delete(stored.id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryRefreshTokenRepository;
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
        let repo = Arc::new(InMemoryRefreshTokenRepository::default());
        (
            RefreshSession::new(repo.clone(), Arc::new(FakeTokens), 30),
            repo,
        )
    }

    async fn seeded_token(repo: &InMemoryRefreshTokenRepository, ttl_days: i64) -> (Uuid, String) {
        let user_id = Uuid::new_v4();
        let (record, plaintext) = RefreshToken::issue(user_id, ttl_days);
        repo.insert(&record).await.unwrap();
        (user_id, plaintext)
    }

    #[tokio::test]
    async fn rotation_issues_a_new_pair_and_consumes_the_old_token() {
        let (service, repo) = service();
        let (user_id, plaintext) = seeded_token(&repo, 30).await;

        let tokens = service.rotate(&plaintext).await.unwrap();
        assert_eq!(tokens.access_token, format!("token-for:{user_id}"));
        assert_ne!(tokens.refresh_token, plaintext);

        // Replaying the consumed token fails (single use).
        let err = service.rotate(&plaintext).await.unwrap_err();
        assert!(matches!(err, RefreshError::InvalidToken));

        // The freshly issued token works.
        service.rotate(&tokens.refresh_token).await.unwrap();
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
