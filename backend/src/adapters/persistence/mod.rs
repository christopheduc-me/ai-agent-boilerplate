//! Persistence adapters.
//!
//! - `postgres`: the production adapter (sqlx, ADR-007). Migrations run at
//!   startup; integration tests live in `tests/postgres_repositories.rs` and
//!   run against any PostgreSQL pointed to by `DATABASE_URL` (the compose
//!   service locally, a GitLab service in CI — ADR-012/015).
//! - `in_memory`: fake used by unit tests and as a fallback when
//!   `DATABASE_URL` is not set (data is lost on restart).
pub mod in_memory;
pub mod postgres;
