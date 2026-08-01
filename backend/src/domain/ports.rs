//! Ports (hexagonal architecture): the domain and use cases depend only on these
//! traits. Adapters (persistence, auth, dispatch) implement them.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    AgentStep, JobUsage, RecurringSearch, RefreshToken, ResearchJob, SearchResult, SecurityEvent,
    User,
};

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
    /// Accumulates one task attempt's spend onto the job (ADR-038).
    async fn add_usage(&self, job_id: Uuid, usage: &JobUsage) -> Result<(), PortError>;
    /// URLs already delivered by previous runs of a recurring search — the
    /// memory the agent receives to flag deltas (ADR-033). Most recent first,
    /// capped by `limit` to bound the task payload.
    async fn recent_urls_for_recurring(
        &self,
        recurring_search_id: Uuid,
        limit: u32,
    ) -> Result<Vec<String>, PortError>;
}

/// Saved searches re-run by the scheduler (ADR-033).
#[async_trait]
pub trait RecurringSearchRepository: Send + Sync {
    async fn insert(&self, search: &RecurringSearch) -> Result<(), PortError>;
    async fn find(&self, id: Uuid) -> Result<Option<RecurringSearch>, PortError>;
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<RecurringSearch>, PortError>;
    /// Deletes the user's recurring search; false when unknown or foreign.
    async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<bool, PortError>;
    /// Every recurring search due at `now` (never run, or interval elapsed).
    async fn list_due(&self, now: DateTime<Utc>) -> Result<Vec<RecurringSearch>, PortError>;
    async fn mark_ran(&self, id: Uuid, at: DateTime<Utc>) -> Result<(), PortError>;
}

/// Persisted refresh tokens (ADR-008): stored hashed, single use (rotation).
#[async_trait]
pub trait RefreshTokenRepository: Send + Sync {
    async fn insert(&self, token: &RefreshToken) -> Result<(), PortError>;
    async fn find_by_hash(&self, hash: &str) -> Result<Option<RefreshToken>, PortError>;
    /// Marks a rotated-away token consumed (ADR-056): it is kept, not deleted,
    /// so a later replay is caught as reuse instead of looking unknown.
    async fn mark_consumed(&self, id: Uuid, at: DateTime<Utc>) -> Result<(), PortError>;
    async fn delete(&self, id: Uuid) -> Result<(), PortError>;
    /// Revokes an entire rotation lineage (ADR-056): called on reuse detection
    /// to kill the stolen token's whole family in one shot.
    async fn delete_family(&self, family_id: Uuid) -> Result<(), PortError>;
    /// Purges expired tokens (called by the background reaper).
    async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, PortError>;
}

/// Append-only security audit log (ADR-057): failed/throttled logins, refresh
/// reuse, quota hits. Writes are best-effort — a recording failure must never
/// break the request that triggered it (the caller logs and continues).
#[async_trait]
pub trait SecurityAudit: Send + Sync {
    async fn record(&self, event: &SecurityEvent) -> Result<(), PortError>;
    /// Most recent events first, capped at `limit` — for an operator view.
    async fn list_recent(&self, limit: i64) -> Result<Vec<SecurityEvent>, PortError>;
    /// Retention purge (ADR-057): drops events older than `cutoff`. Called by
    /// the background loop, like the refresh-token purge.
    async fn delete_before(&self, cutoff: DateTime<Utc>) -> Result<u64, PortError>;
}

/// Sends a research job to the agent (via the FastAPI micro-API, see ADR-005).
/// `seen_urls` is the recurring-search memory (ADR-033): URLs delivered by
/// previous runs, empty for one-shot searches.
#[async_trait]
pub trait JobDispatcher: Send + Sync {
    async fn dispatch(&self, job: &ResearchJob, seen_urls: &[String]) -> Result<(), PortError>;
}

/// A digest of a recurring run that found something new (ADR-036).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Digest {
    pub recurring_search_id: Uuid,
    pub job_id: Uuid,
    pub keyword: String,
    pub new_count: usize,
    /// The new results only (title/url/published_at), newest first.
    pub new_results: Vec<DigestEntry>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DigestEntry {
    pub title: String,
    pub url: String,
    pub published_at: Option<chrono::DateTime<Utc>>,
}

/// Delivers digests (ADR-036) — webhook in this repository; an e-mail sender
/// is one more adapter behind the same port. Best-effort by contract: a
/// failed delivery is logged, never fails the ingestion.
#[async_trait]
pub trait DigestSender: Send + Sync {
    async fn send(&self, webhook_url: &str, digest: &Digest) -> Result<(), PortError>;
}

pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: &str) -> Result<String, PortError>;
    fn verify(&self, password: &str, hash: &str) -> bool;
}

pub trait TokenService: Send + Sync {
    fn issue(&self, user_id: Uuid) -> Result<String, PortError>;
    fn verify(&self, token: &str) -> Option<Uuid>;
}
