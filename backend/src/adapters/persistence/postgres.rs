//! PostgreSQL implementations of the persistence ports (ADR-007, sqlx).
//!
//! Queries are runtime-checked (`sqlx::query`) rather than macro-checked so the
//! project compiles without a database; the integration tests in
//! `tests/postgres_repositories.rs` exercise them against a real PostgreSQL.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::adapters::leader_lock::LeaderLock;
use crate::domain::ports::{
    JobRepository, PortError, RecurringSearchRepository, RefreshTokenRepository, SecurityAudit,
    UserRepository,
};
use crate::domain::SecurityEvent;

/// Advisory-lock key for the background loop (ADR-053): a fixed application id
/// so every replica contends on the same lock.
const SCHEDULER_LOCK_KEY: i64 = 918_273_645;

/// A `LeaderLock` backed by a PostgreSQL **session advisory lock**
/// (`pg_try_advisory_lock`). Only one replica can hold the key at a time, so
/// only one runs the background loop per tick (ADR-053). The connection that
/// took the lock is held until `release`, because a session advisory lock must
/// be unlocked on the same connection — and it is returned to the pool between
/// ticks, so the lock can rotate. If the process dies, the session ends and the
/// lock frees automatically.
pub struct PostgresLeaderLock {
    pool: PgPool,
    key: i64,
    held: Mutex<Option<PoolConnection<Postgres>>>,
}

impl PostgresLeaderLock {
    pub fn new(pool: PgPool) -> Self {
        Self::with_key(pool, SCHEDULER_LOCK_KEY)
    }

    /// A leader lock on a specific advisory key — for independent background
    /// loops, or an isolated key in tests.
    pub fn with_key(pool: PgPool, key: i64) -> Self {
        Self {
            pool,
            key,
            held: Mutex::new(None),
        }
    }
}

#[async_trait]
impl LeaderLock for PostgresLeaderLock {
    async fn acquire(&self) -> bool {
        let mut conn = match self.pool.acquire().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!(error = %e, "leader lock: cannot acquire a connection");
                return false;
            }
        };
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(self.key)
            .fetch_one(&mut *conn)
            .await
            .unwrap_or(false);
        if acquired {
            *self.held.lock().await = Some(conn);
        }
        acquired
    }

    async fn release(&self) {
        if let Some(mut conn) = self.held.lock().await.take() {
            if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(self.key)
                .execute(&mut *conn)
                .await
            {
                tracing::error!(error = %e, "leader lock: advisory unlock failed");
            }
        }
    }
}
use crate::domain::{
    AgentStep, DateConfidence, EventType, JobMode, JobStatus, JobUsage, RecurringSearch,
    RefreshToken, ResearchJob, SearchResult, User,
};

/// Runs the SQL migrations in `backend/migrations/` (idempotent).
pub async fn run_migrations(pool: &PgPool) -> Result<(), PortError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| PortError(format!("migrations failed: {e}")))
}

fn db_err(e: sqlx::Error) -> PortError {
    PortError(format!("database error: {e}"))
}

fn status_to_str(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Pending => "pending",
        JobStatus::Running => "running",
        JobStatus::AwaitingInput => "awaiting_input",
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
    }
}

fn status_from_str(value: &str) -> Result<JobStatus, PortError> {
    match value {
        "pending" => Ok(JobStatus::Pending),
        "running" => Ok(JobStatus::Running),
        "awaiting_input" => Ok(JobStatus::AwaitingInput),
        "completed" => Ok(JobStatus::Completed),
        "failed" => Ok(JobStatus::Failed),
        other => Err(PortError(format!(
            "unknown job status in database: {other}"
        ))),
    }
}

fn mode_to_str(mode: JobMode) -> &'static str {
    match mode {
        JobMode::Workflow => "workflow",
        JobMode::Agent => "agent",
    }
}

/// Unknown values degrade to the default mode (forward compatibility).
fn mode_from_str(value: &str) -> JobMode {
    match value {
        "agent" => JobMode::Agent,
        _ => JobMode::Workflow,
    }
}

fn confidence_to_str(confidence: DateConfidence) -> &'static str {
    match confidence {
        DateConfidence::High => "high",
        DateConfidence::Medium => "medium",
        DateConfidence::Unknown => "unknown",
    }
}

fn confidence_from_str(value: &str) -> Result<DateConfidence, PortError> {
    match value {
        "high" => Ok(DateConfidence::High),
        "medium" => Ok(DateConfidence::Medium),
        "unknown" => Ok(DateConfidence::Unknown),
        other => Err(PortError(format!(
            "unknown date confidence in database: {other}"
        ))),
    }
}

fn user_from_row(row: &PgRow) -> User {
    User {
        id: row.get("id"),
        email: row.get("email"),
        password_hash: row.get("password_hash"),
        created_at: row.get("created_at"),
    }
}

fn job_from_row(row: &PgRow) -> Result<ResearchJob, PortError> {
    Ok(ResearchJob {
        id: row.get("id"),
        user_id: row.get("user_id"),
        keyword: row.get("keyword"),
        mode: mode_from_str(row.get("mode")),
        status: status_from_str(row.get("status"))?,
        error: row.get("error"),
        question: row.get("question"),
        answer: row.get("answer"),
        recurring_search_id: row.get("recurring_search_id"),
        usage: JobUsage {
            llm_calls: row.get("llm_calls"),
            llm_input_tokens: row.get("llm_input_tokens"),
            llm_output_tokens: row.get("llm_output_tokens"),
            search_calls: row.get("search_calls"),
            cost_usd: row.get("cost_usd"),
        },
        created_at: row.get("created_at"),
        completed_at: row.get("completed_at"),
    })
}

fn event_type_to_str(event_type: EventType) -> &'static str {
    match event_type {
        EventType::Announcement => "announcement",
        EventType::Release => "release",
        EventType::Funding => "funding",
        EventType::Legal => "legal",
        EventType::Incident => "incident",
        EventType::Research => "research",
        EventType::Opinion => "opinion",
        EventType::Other => "other",
    }
}

/// Unknown values degrade to `Other` (forward compatibility: a newer agent may
/// write event types this backend version does not know yet).
fn event_type_from_str(value: &str) -> EventType {
    match value {
        "announcement" => EventType::Announcement,
        "release" => EventType::Release,
        "funding" => EventType::Funding,
        "legal" => EventType::Legal,
        "incident" => EventType::Incident,
        "research" => EventType::Research,
        "opinion" => EventType::Opinion,
        _ => EventType::Other,
    }
}

fn result_from_row(row: &PgRow) -> Result<SearchResult, PortError> {
    Ok(SearchResult {
        title: row.get("title"),
        url: row.get("url"),
        snippet: row.get("snippet"),
        published_at: row.get("published_at"),
        date_confidence: confidence_from_str(row.get("date_confidence"))?,
        event_type: event_type_from_str(row.get("event_type")),
        summary: row.get("summary"),
        is_new: row.get("is_new"),
        raw: row.get("raw"),
    })
}

// ---------------------------------------------------------------- users

pub struct PostgresUserRepository {
    pool: PgPool,
}

impl PostgresUserRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserRepository for PostgresUserRepository {
    async fn insert(&self, user: &User) -> Result<(), PortError> {
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(user.id)
        .bind(&user.email)
        .bind(&user.password_hash)
        .bind(user.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<User>, PortError> {
        let row =
            sqlx::query("SELECT id, email, password_hash, created_at FROM users WHERE email = $1")
                .bind(email)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.as_ref().map(user_from_row))
    }
}

// ---------------------------------------------------------------- refresh tokens

pub struct PostgresRefreshTokenRepository {
    pool: PgPool,
}

impl PostgresRefreshTokenRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RefreshTokenRepository for PostgresRefreshTokenRepository {
    async fn insert(&self, token: &RefreshToken) -> Result<(), PortError> {
        sqlx::query(
            "INSERT INTO refresh_tokens
                 (id, user_id, family_id, token_hash, expires_at, created_at, consumed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(token.id)
        .bind(token.user_id)
        .bind(token.family_id)
        .bind(&token.token_hash)
        .bind(token.expires_at)
        .bind(token.created_at)
        .bind(token.consumed_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn find_by_hash(&self, hash: &str) -> Result<Option<RefreshToken>, PortError> {
        let row = sqlx::query(
            "SELECT id, user_id, family_id, token_hash, expires_at, created_at, consumed_at
             FROM refresh_tokens WHERE token_hash = $1",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|row| RefreshToken {
            id: row.get("id"),
            user_id: row.get("user_id"),
            family_id: row.get("family_id"),
            token_hash: row.get("token_hash"),
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
            consumed_at: row.get("consumed_at"),
        }))
    }

    async fn mark_consumed(&self, id: Uuid, at: DateTime<Utc>) -> Result<(), PortError> {
        sqlx::query("UPDATE refresh_tokens SET consumed_at = $2 WHERE id = $1")
            .bind(id)
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<(), PortError> {
        sqlx::query("DELETE FROM refresh_tokens WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn delete_family(&self, family_id: Uuid) -> Result<(), PortError> {
        sqlx::query("DELETE FROM refresh_tokens WHERE family_id = $1")
            .bind(family_id)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, PortError> {
        let result = sqlx::query("DELETE FROM refresh_tokens WHERE expires_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected())
    }
}

// ---------------------------------------------------------------- jobs + results

pub struct PostgresJobRepository {
    pool: PgPool,
}

impl PostgresJobRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobRepository for PostgresJobRepository {
    async fn insert(&self, job: &ResearchJob) -> Result<(), PortError> {
        sqlx::query(
            "INSERT INTO research_jobs (id, user_id, keyword, mode, status, error, question, answer, recurring_search_id, created_at, completed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(job.id)
        .bind(job.user_id)
        .bind(&job.keyword)
        .bind(mode_to_str(job.mode))
        .bind(status_to_str(job.status))
        .bind(&job.error)
        .bind(&job.question)
        .bind(&job.answer)
        .bind(job.recurring_search_id)
        .bind(job.created_at)
        .bind(job.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn update(&self, job: &ResearchJob) -> Result<(), PortError> {
        sqlx::query(
            "UPDATE research_jobs
             SET status = $2, error = $3, question = $4, answer = $5, completed_at = $6
             WHERE id = $1",
        )
        .bind(job.id)
        .bind(status_to_str(job.status))
        .bind(&job.error)
        .bind(&job.question)
        .bind(&job.answer)
        .bind(job.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn find(&self, id: Uuid) -> Result<Option<ResearchJob>, PortError> {
        let row = sqlx::query(
            "SELECT id, user_id, keyword, mode, status, error, question, answer, recurring_search_id,
                    llm_calls, llm_input_tokens, llm_output_tokens, search_calls, cost_usd,
                    created_at, completed_at
             FROM research_jobs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.as_ref().map(job_from_row).transpose()
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ResearchJob>, PortError> {
        let rows = sqlx::query(
            "SELECT id, user_id, keyword, mode, status, error, question, answer, recurring_search_id,
                    llm_calls, llm_input_tokens, llm_output_tokens, search_calls, cost_usd,
                    created_at, completed_at
             FROM research_jobs WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter().map(job_from_row).collect()
    }

    async fn count_created_since(
        &self,
        user_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<u64, PortError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM research_jobs WHERE user_id = $1 AND created_at >= $2",
        )
        .bind(user_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        let n: i64 = row.get("n");
        Ok(n as u64)
    }

    async fn list_unfinished_older_than(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<Vec<ResearchJob>, PortError> {
        let rows = sqlx::query(
            "SELECT id, user_id, keyword, mode, status, error, question, answer, recurring_search_id,
                    llm_calls, llm_input_tokens, llm_output_tokens, search_calls, cost_usd,
                    created_at, completed_at
             FROM research_jobs
             WHERE status IN ('pending', 'running') AND created_at < $1",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter().map(job_from_row).collect()
    }

    async fn store_results(&self, job_id: Uuid, results: &[SearchResult]) -> Result<(), PortError> {
        // Replace semantics inside one transaction: re-delivery by the worker
        // (Celery retry) must not duplicate results.
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("DELETE FROM search_results WHERE job_id = $1")
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        for result in results {
            sqlx::query(
                "INSERT INTO search_results (job_id, title, url, snippet, published_at, date_confidence, event_type, summary, is_new, raw)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(job_id)
            .bind(&result.title)
            .bind(&result.url)
            .bind(&result.snippet)
            .bind(result.published_at)
            .bind(confidence_to_str(result.date_confidence))
            .bind(event_type_to_str(result.event_type))
            .bind(&result.summary)
            .bind(result.is_new)
            .bind(&result.raw)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)
    }

    async fn results_for(&self, job_id: Uuid) -> Result<Vec<SearchResult>, PortError> {
        let rows = sqlx::query(
            "SELECT title, url, snippet, published_at, date_confidence, event_type, summary, is_new, raw
             FROM search_results WHERE job_id = $1
             ORDER BY published_at DESC NULLS LAST",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter().map(result_from_row).collect()
    }

    async fn append_step(&self, job_id: Uuid, step: &AgentStep) -> Result<(), PortError> {
        // Idempotent on (job_id, seq): a Celery retry re-sends the same step.
        sqlx::query(
            "INSERT INTO agent_steps (job_id, seq, kind, detail, reason, new_hits)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (job_id, seq) DO NOTHING",
        )
        .bind(job_id)
        .bind(step.seq)
        .bind(&step.kind)
        .bind(&step.detail)
        .bind(&step.reason)
        .bind(step.new_hits)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn steps_for(&self, job_id: Uuid) -> Result<Vec<AgentStep>, PortError> {
        let rows = sqlx::query(
            "SELECT seq, kind, detail, reason, new_hits
             FROM agent_steps WHERE job_id = $1 ORDER BY seq",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|row| AgentStep {
                seq: row.get("seq"),
                kind: row.get("kind"),
                detail: row.get("detail"),
                reason: row.get("reason"),
                new_hits: row.get("new_hits"),
            })
            .collect())
    }

    async fn clear_steps(&self, job_id: Uuid) -> Result<(), PortError> {
        sqlx::query("DELETE FROM agent_steps WHERE job_id = $1")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn add_usage(&self, job_id: Uuid, usage: &JobUsage) -> Result<(), PortError> {
        sqlx::query(
            "UPDATE research_jobs
             SET llm_calls = llm_calls + $2,
                 llm_input_tokens = llm_input_tokens + $3,
                 llm_output_tokens = llm_output_tokens + $4,
                 search_calls = search_calls + $5,
                 cost_usd = cost_usd + $6
             WHERE id = $1",
        )
        .bind(job_id)
        .bind(usage.llm_calls)
        .bind(usage.llm_input_tokens)
        .bind(usage.llm_output_tokens)
        .bind(usage.search_calls)
        .bind(usage.cost_usd)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn recent_urls_for_recurring(
        &self,
        recurring_search_id: Uuid,
        limit: u32,
    ) -> Result<Vec<String>, PortError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (r.url) r.url, j.created_at
             FROM search_results r
             JOIN research_jobs j ON j.id = r.job_id
             WHERE j.recurring_search_id = $1
             ORDER BY r.url, j.created_at DESC
             LIMIT $2",
        )
        .bind(recurring_search_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(|row| row.get("url")).collect())
    }
}

// ---------------------------------------------------------------- recurring searches (ADR-033)

pub struct PostgresRecurringSearchRepository {
    pool: PgPool,
}

impl PostgresRecurringSearchRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn recurring_from_row(row: &PgRow) -> RecurringSearch {
    let interval: i32 = row.get("interval_minutes");
    RecurringSearch {
        id: row.get("id"),
        user_id: row.get("user_id"),
        keyword: row.get("keyword"),
        mode: mode_from_str(row.get("mode")),
        interval_minutes: interval as u32,
        webhook_url: row.get("webhook_url"),
        created_at: row.get("created_at"),
        last_run_at: row.get("last_run_at"),
    }
}

const RECURRING_COLS: &str =
    "SELECT id, user_id, keyword, mode, interval_minutes, webhook_url, created_at, last_run_at
     FROM recurring_searches";

#[async_trait]
impl RecurringSearchRepository for PostgresRecurringSearchRepository {
    async fn insert(&self, search: &RecurringSearch) -> Result<(), PortError> {
        sqlx::query(
            "INSERT INTO recurring_searches
             (id, user_id, keyword, mode, interval_minutes, webhook_url, created_at, last_run_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(search.id)
        .bind(search.user_id)
        .bind(&search.keyword)
        .bind(mode_to_str(search.mode))
        .bind(search.interval_minutes as i32)
        .bind(&search.webhook_url)
        .bind(search.created_at)
        .bind(search.last_run_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn find(&self, id: Uuid) -> Result<Option<RecurringSearch>, PortError> {
        let row = sqlx::query(&format!("{RECURRING_COLS} WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(recurring_from_row))
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<RecurringSearch>, PortError> {
        let rows = sqlx::query(&format!(
            "{RECURRING_COLS} WHERE user_id = $1 ORDER BY created_at DESC"
        ))
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(recurring_from_row).collect())
    }

    async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<bool, PortError> {
        let result = sqlx::query("DELETE FROM recurring_searches WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_due(&self, now: DateTime<Utc>) -> Result<Vec<RecurringSearch>, PortError> {
        let rows = sqlx::query(&format!(
            "{RECURRING_COLS}
             WHERE last_run_at IS NULL
                OR last_run_at + make_interval(mins => interval_minutes) <= $1"
        ))
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(recurring_from_row).collect())
    }

    async fn mark_ran(&self, id: Uuid, at: DateTime<Utc>) -> Result<(), PortError> {
        sqlx::query("UPDATE recurring_searches SET last_run_at = $2 WHERE id = $1")
            .bind(id)
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}

// ---------------------------------------------------------------- security audit

pub struct PostgresSecurityAudit {
    pool: PgPool,
}

impl PostgresSecurityAudit {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn security_event_from_row(row: &PgRow) -> SecurityEvent {
    SecurityEvent {
        id: row.get("id"),
        kind: row.get("kind"),
        user_id: row.get("user_id"),
        client_ip: row.get("client_ip"),
        detail: row.get("detail"),
        created_at: row.get("created_at"),
    }
}

#[async_trait]
impl SecurityAudit for PostgresSecurityAudit {
    async fn record(&self, event: &SecurityEvent) -> Result<(), PortError> {
        sqlx::query(
            "INSERT INTO security_events (id, kind, user_id, client_ip, detail, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(event.id)
        .bind(&event.kind)
        .bind(event.user_id)
        .bind(&event.client_ip)
        .bind(&event.detail)
        .bind(event.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn list_recent(&self, limit: i64) -> Result<Vec<SecurityEvent>, PortError> {
        let rows = sqlx::query(
            "SELECT id, kind, user_id, client_ip, detail, created_at
             FROM security_events ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(security_event_from_row).collect())
    }

    async fn delete_before(&self, cutoff: DateTime<Utc>) -> Result<u64, PortError> {
        let result = sqlx::query("DELETE FROM security_events WHERE created_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected())
    }
}
