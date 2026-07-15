//! Ports (hexagonal architecture): the domain and use cases depend only on these
//! traits. Adapters (persistence, auth, dispatch) implement them.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{AgentStep, RefreshToken, ResearchJob, SearchResult, User};

/// Infrastructure failure surfaced through a port (DB down, network error...).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PortError(pub String);

#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn insert(&self, user: &User) -> Result<(), PortError>;
    async fn find_by_email(&self, email: &str) -> Result<Option<User>, PortError>;
}

#[async_trait]
pub trait JobRepository: Send + Sync {
    async fn insert(&self, job: &ResearchJob) -> Result<(), PortError>;
    async fn update(&self, job: &ResearchJob) -> Result<(), PortError>;
    async fn find(&self, id: Uuid) -> Result<Option<ResearchJob>, PortError>;
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ResearchJob>, PortError>;
    /// Number of jobs the user created since `since` — quota input (ADR-017).
    async fn count_created_since(
        &self,
        user_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<u64, PortError>;
    /// Unfinished (pending/running) jobs created before `cutoff` — reaper input.
    async fn list_unfinished_older_than(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<Vec<ResearchJob>, PortError>;
    async fn store_results(&self, job_id: Uuid, results: &[SearchResult]) -> Result<(), PortError>;
    async fn results_for(&self, job_id: Uuid) -> Result<Vec<SearchResult>, PortError>;
    /// Records one decision of the agentic loop (ADR-030). Idempotent on
    /// `(job_id, seq)` so Celery retries never duplicate journal entries.
    async fn append_step(&self, job_id: Uuid, step: &AgentStep) -> Result<(), PortError>;
    /// The journal in `seq` order.
    async fn steps_for(&self, job_id: Uuid) -> Result<Vec<AgentStep>, PortError>;
    /// Replace semantics on resume (ADR-032): answering a clarification
    /// re-runs the loop from scratch, so the journal starts fresh too.
    async fn clear_steps(&self, job_id: Uuid) -> Result<(), PortError>;
}

/// Persisted refresh tokens (ADR-008): stored hashed, single use (rotation).
#[async_trait]
pub trait RefreshTokenRepository: Send + Sync {
    async fn insert(&self, token: &RefreshToken) -> Result<(), PortError>;
    async fn find_by_hash(&self, hash: &str) -> Result<Option<RefreshToken>, PortError>;
    async fn delete(&self, id: Uuid) -> Result<(), PortError>;
    /// Purges expired tokens (called by the background reaper).
    async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, PortError>;
}

/// Sends a research job to the agent (via the FastAPI micro-API, see ADR-005).
#[async_trait]
pub trait JobDispatcher: Send + Sync {
    async fn dispatch(&self, job: &ResearchJob) -> Result<(), PortError>;
}

pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: &str) -> Result<String, PortError>;
    fn verify(&self, password: &str, hash: &str) -> bool;
}

pub trait TokenService: Send + Sync {
    fn issue(&self, user_id: Uuid) -> Result<String, PortError>;
    fn verify(&self, token: &str) -> Option<Uuid>;
}
