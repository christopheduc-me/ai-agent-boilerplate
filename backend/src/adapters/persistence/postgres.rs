//! PostgreSQL implementations of the persistence ports (ADR-007, sqlx).
//!
//! Queries are runtime-checked (`sqlx::query`) rather than macro-checked so the
//! project compiles without a database; the integration tests in
//! `tests/postgres_repositories.rs` exercise them against a real PostgreSQL.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::ports::{JobRepository, PortError, RefreshTokenRepository, UserRepository};
use crate::domain::{
    DateConfidence, EventType, JobStatus, RefreshToken, ResearchJob, SearchResult, User,
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
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
    }
}

fn status_from_str(value: &str) -> Result<JobStatus, PortError> {
    match value {
        "pending" => Ok(JobStatus::Pending),
        "running" => Ok(JobStatus::Running),
        "completed" => Ok(JobStatus::Completed),
        "failed" => Ok(JobStatus::Failed),
        other => Err(PortError(format!(
            "unknown job status in database: {other}"
        ))),
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
        status: status_from_str(row.get("status"))?,
        error: row.get("error"),
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
            "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at, created_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(token.id)
        .bind(token.user_id)
        .bind(&token.token_hash)
        .bind(token.expires_at)
        .bind(token.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn find_by_hash(&self, hash: &str) -> Result<Option<RefreshToken>, PortError> {
        let row = sqlx::query(
            "SELECT id, user_id, token_hash, expires_at, created_at
             FROM refresh_tokens WHERE token_hash = $1",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|row| RefreshToken {
            id: row.get("id"),
            user_id: row.get("user_id"),
            token_hash: row.get("token_hash"),
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
        }))
    }

    async fn delete(&self, id: Uuid) -> Result<(), PortError> {
        sqlx::query("DELETE FROM refresh_tokens WHERE id = $1")
            .bind(id)
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
            "INSERT INTO research_jobs (id, user_id, keyword, status, error, created_at, completed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(job.id)
        .bind(job.user_id)
        .bind(&job.keyword)
        .bind(status_to_str(job.status))
        .bind(&job.error)
        .bind(job.created_at)
        .bind(job.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn update(&self, job: &ResearchJob) -> Result<(), PortError> {
        sqlx::query(
            "UPDATE research_jobs SET status = $2, error = $3, completed_at = $4 WHERE id = $1",
        )
        .bind(job.id)
        .bind(status_to_str(job.status))
        .bind(&job.error)
        .bind(job.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn find(&self, id: Uuid) -> Result<Option<ResearchJob>, PortError> {
        let row = sqlx::query(
            "SELECT id, user_id, keyword, status, error, created_at, completed_at
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
            "SELECT id, user_id, keyword, status, error, created_at, completed_at
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
            "SELECT id, user_id, keyword, status, error, created_at, completed_at
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
                "INSERT INTO search_results (job_id, title, url, snippet, published_at, date_confidence, event_type, summary, raw)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(job_id)
            .bind(&result.title)
            .bind(&result.url)
            .bind(&result.snippet)
            .bind(result.published_at)
            .bind(confidence_to_str(result.date_confidence))
            .bind(event_type_to_str(result.event_type))
            .bind(&result.summary)
            .bind(&result.raw)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)
    }

    async fn results_for(&self, job_id: Uuid) -> Result<Vec<SearchResult>, PortError> {
        let rows = sqlx::query(
            "SELECT title, url, snippet, published_at, date_confidence, event_type, summary, raw
             FROM search_results WHERE job_id = $1
             ORDER BY published_at DESC NULLS LAST",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter().map(result_from_row).collect()
    }
}
