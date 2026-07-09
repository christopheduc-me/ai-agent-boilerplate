//! Authentication adapters (ADR-008): argon2id password hashing + JWT access tokens.
//!
//! Next TDD step: refresh-token rotation (`/api/auth/refresh`, `/api/auth/logout`)
//! with hashed refresh tokens persisted for revocation.

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher as _, PasswordVerifier as _};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::ports::{PasswordHasher, PortError, TokenService};

pub struct Argon2PasswordHasher;

impl PasswordHasher for Argon2PasswordHasher {
    fn hash(&self, password: &str) -> Result<String, PortError> {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| PortError(format!("argon2 hashing failed: {e}")))
    }

    fn verify(&self, password: &str, hash: &str) -> bool {
        PasswordHash::new(hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false)
    }
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    iat: i64,
    exp: i64,
}

pub struct JwtTokenService {
    encoding: EncodingKey,
    decoding: DecodingKey,
    ttl: Duration,
}

impl JwtTokenService {
    pub fn new(secret: &str, ttl_minutes: i64) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            ttl: Duration::minutes(ttl_minutes),
        }
    }
}

impl TokenService for JwtTokenService {
    fn issue(&self, user_id: Uuid) -> Result<String, PortError> {
        let now = Utc::now();
        let claims = Claims {
            sub: user_id.to_string(),
            iat: now.timestamp(),
            exp: (now + self.ttl).timestamp(),
        };
        encode(&Header::default(), &claims, &self.encoding)
            .map_err(|e| PortError(format!("jwt signing failed: {e}")))
    }

    fn verify(&self, token: &str) -> Option<Uuid> {
        let data = decode::<Claims>(token, &self.decoding, &Validation::default()).ok()?;
        Uuid::parse_str(&data.claims.sub).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_roundtrip() {
        let hasher = Argon2PasswordHasher;
        let hash = hasher.hash("correct horse battery staple").unwrap();
        assert!(hasher.verify("correct horse battery staple", &hash));
        assert!(!hasher.verify("wrong password", &hash));
    }

    #[test]
    fn jwt_roundtrip_and_tamper_resistance() {
        let service = JwtTokenService::new("test-secret", 15);
        let user_id = Uuid::new_v4();

        let token = service.issue(user_id).unwrap();
        assert_eq!(service.verify(&token), Some(user_id));

        let other = JwtTokenService::new("another-secret", 15);
        assert_eq!(other.verify(&token), None);
    }
}
