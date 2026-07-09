use std::sync::Arc;

use crate::domain::ports::{PasswordHasher, PortError, UserRepository};
use crate::domain::User;

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("email already registered")]
    EmailTaken,
    #[error("invalid email address")]
    InvalidEmail,
    #[error("password must be at least 8 characters")]
    PasswordTooShort,
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

pub struct RegisterUser {
    users: Arc<dyn UserRepository>,
    hasher: Arc<dyn PasswordHasher>,
}

impl RegisterUser {
    pub fn new(users: Arc<dyn UserRepository>, hasher: Arc<dyn PasswordHasher>) -> Self {
        Self { users, hasher }
    }

    pub async fn execute(&self, email: &str, password: &str) -> Result<User, RegisterError> {
        let email = email.trim().to_lowercase();
        if !email.contains('@') || email.len() < 3 {
            return Err(RegisterError::InvalidEmail);
        }
        if password.len() < 8 {
            return Err(RegisterError::PasswordTooShort);
        }
        if self.users.find_by_email(&email).await?.is_some() {
            return Err(RegisterError::EmailTaken);
        }
        let user = User::new(email, self.hasher.hash(password)?);
        self.users.insert(&user).await?;
        Ok(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryUserRepository;

    struct FakeHasher;

    impl PasswordHasher for FakeHasher {
        fn hash(&self, password: &str) -> Result<String, PortError> {
            Ok(format!("hashed:{password}"))
        }
        fn verify(&self, password: &str, hash: &str) -> bool {
            hash == format!("hashed:{password}")
        }
    }

    fn use_case() -> (RegisterUser, Arc<InMemoryUserRepository>) {
        let users = Arc::new(InMemoryUserRepository::default());
        (
            RegisterUser::new(users.clone(), Arc::new(FakeHasher)),
            users,
        )
    }

    #[tokio::test]
    async fn registers_a_user_with_hashed_password() {
        let (register, users) = use_case();

        let user = register
            .execute("Alice@Example.com", "s3cret-pw")
            .await
            .unwrap();

        assert_eq!(user.email, "alice@example.com");
        assert_eq!(user.password_hash, "hashed:s3cret-pw");
        let stored = users.find_by_email("alice@example.com").await.unwrap();
        assert_eq!(stored, Some(user));
    }

    #[tokio::test]
    async fn rejects_duplicate_email() {
        let (register, _) = use_case();
        register.execute("a@b.com", "password1").await.unwrap();

        let err = register.execute("a@b.com", "password2").await.unwrap_err();
        assert!(matches!(err, RegisterError::EmailTaken));
    }

    #[tokio::test]
    async fn rejects_short_password() {
        let (register, _) = use_case();
        let err = register.execute("a@b.com", "short").await.unwrap_err();
        assert!(matches!(err, RegisterError::PasswordTooShort));
    }

    #[tokio::test]
    async fn rejects_invalid_email() {
        let (register, _) = use_case();
        let err = register
            .execute("not-an-email", "password1")
            .await
            .unwrap_err();
        assert!(matches!(err, RegisterError::InvalidEmail));
    }
}
