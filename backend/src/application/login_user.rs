use std::sync::Arc;

use crate::domain::ports::{
    PasswordHasher, PortError, RefreshTokenRepository, TokenService, UserRepository,
};
use crate::domain::RefreshToken;

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

/// What a successful authentication hands back (ADR-008): a short-lived JWT
/// for the Authorization header and a single-use refresh token for the cookie.
#[derive(Debug)]
pub struct SessionTokens {
    pub access_token: String,
    pub refresh_token: String,
}

pub struct LoginUser {
    users: Arc<dyn UserRepository>,
    hasher: Arc<dyn PasswordHasher>,
    tokens: Arc<dyn TokenService>,
    refresh_tokens: Arc<dyn RefreshTokenRepository>,
    refresh_ttl_days: i64,
}

impl LoginUser {
    pub fn new(
        users: Arc<dyn UserRepository>,
        hasher: Arc<dyn PasswordHasher>,
        tokens: Arc<dyn TokenService>,
        refresh_tokens: Arc<dyn RefreshTokenRepository>,
        refresh_ttl_days: i64,
    ) -> Self {
        Self {
            users,
            hasher,
            tokens,
            refresh_tokens,
            refresh_ttl_days,
        }
    }

    pub async fn execute(&self, email: &str, password: &str) -> Result<SessionTokens, LoginError> {
        let email = email.trim().to_lowercase();
        let user = self
            .users
            .find_by_email(&email)
            .await?
            .ok_or(LoginError::InvalidCredentials)?;
        if !self.hasher.verify(password, &user.password_hash) {
            return Err(LoginError::InvalidCredentials);
        }

        let (record, plaintext) = RefreshToken::issue(user.id, self.refresh_ttl_days);
        self.refresh_tokens.insert(&record).await?;
        Ok(SessionTokens {
            access_token: self.tokens.issue(user.id)?,
            refresh_token: plaintext,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::{
        InMemoryRefreshTokenRepository, InMemoryUserRepository,
    };
    use crate::domain::User;
    use uuid::Uuid;

    pub(crate) struct FakeHasher;
    impl PasswordHasher for FakeHasher {
        fn hash(&self, password: &str) -> Result<String, PortError> {
            Ok(format!("hashed:{password}"))
        }
        fn verify(&self, password: &str, hash: &str) -> bool {
            hash == format!("hashed:{password}")
        }
    }

    pub(crate) struct FakeTokens;
    impl TokenService for FakeTokens {
        fn issue(&self, user_id: Uuid) -> Result<String, PortError> {
            Ok(format!("token-for:{user_id}"))
        }
        fn verify(&self, token: &str) -> Option<Uuid> {
            token
                .strip_prefix("token-for:")
                .and_then(|id| Uuid::parse_str(id).ok())
        }
    }

    async fn login_with_user() -> (LoginUser, Arc<InMemoryRefreshTokenRepository>, User) {
        let users = Arc::new(InMemoryUserRepository::default());
        let refresh = Arc::new(InMemoryRefreshTokenRepository::default());
        let user = User::new("a@b.com".into(), "hashed:good-password".into());
        users.insert(&user).await.unwrap();
        (
            LoginUser::new(
                users,
                Arc::new(FakeHasher),
                Arc::new(FakeTokens),
                refresh.clone(),
                30,
            ),
            refresh,
            user,
        )
    }

    #[tokio::test]
    async fn issues_both_tokens_and_persists_the_refresh_hash() {
        let (login, refresh_repo, user) = login_with_user().await;

        let tokens = login.execute("a@b.com", "good-password").await.unwrap();

        assert_eq!(tokens.access_token, format!("token-for:{}", user.id));
        let stored = refresh_repo
            .find_by_hash(&RefreshToken::hash(&tokens.refresh_token))
            .await
            .unwrap()
            .expect("refresh token must be persisted hashed");
        assert_eq!(stored.user_id, user.id);
    }

    #[tokio::test]
    async fn rejects_wrong_password() {
        let (login, _, _) = login_with_user().await;
        let err = login.execute("a@b.com", "wrong").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidCredentials));
    }

    #[tokio::test]
    async fn rejects_unknown_email() {
        let (login, _, _) = login_with_user().await;
        let err = login
            .execute("nobody@b.com", "good-password")
            .await
            .unwrap_err();
        assert!(matches!(err, LoginError::InvalidCredentials));
    }
}
