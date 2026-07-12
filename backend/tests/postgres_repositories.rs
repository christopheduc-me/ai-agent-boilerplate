//! Integration tests for the PostgreSQL adapter (ADR-007).
//!
//! They run against the database pointed to by `DATABASE_URL` — the compose
//! `postgres` service locally, a GitLab CI service in the pipeline (ADR-012).
//! Without `DATABASE_URL` they are skipped so `cargo test` stays usable offline.
//! Each test works on freshly generated UUIDs, so tests are isolated and can
//! run in parallel against a shared database.

use backend::adapters::persistence::postgres::{
    run_migrations, PostgresJobRepository, PostgresRefreshTokenRepository, PostgresUserRepository,
};
use backend::domain::ports::{JobRepository, RefreshTokenRepository, UserRepository};
use backend::domain::{
    AgentStep, DateConfidence, JobMode, JobStatus, RefreshToken, ResearchJob, SearchResult, User,
};
use chrono::{TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping postgres tests: DATABASE_URL not set");
        return None;
    };
    let pool = PgPool::connect(&url)
        .await
        .expect("cannot connect to DATABASE_URL");
    run_migrations(&pool).await.expect("migrations failed");
    Some(pool)
}

fn unique_email() -> String {
    format!("{}@test.dev", Uuid::new_v4())
}

async fn insert_user(pool: &PgPool) -> User {
    let users = PostgresUserRepository::new(pool.clone());
    let user = User::new(unique_email(), "hash".into());
    users.insert(&user).await.unwrap();
    user
}

#[tokio::test]
async fn user_roundtrip() {
    let Some(pool) = pool().await else { return };
    let users = PostgresUserRepository::new(pool);

    let user = User::new(unique_email(), "argon2-hash".into());
    users.insert(&user).await.unwrap();

    let found = users.find_by_email(&user.email).await.unwrap();
    assert_eq!(found, Some(user));
    assert_eq!(users.find_by_email("nobody@test.dev").await.unwrap(), None);
}

#[tokio::test]
async fn duplicate_email_is_a_database_error() {
    let Some(pool) = pool().await else { return };
    let users = PostgresUserRepository::new(pool);

    let email = unique_email();
    users
        .insert(&User::new(email.clone(), "h1".into()))
        .await
        .unwrap();
    let err = users
        .insert(&User::new(email, "h2".into()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("database error"));
}

#[tokio::test]
async fn job_lifecycle_roundtrip() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool);

    let mut job = ResearchJob::new(user.id, "rust sqlx").unwrap();
    jobs.insert(&job).await.unwrap();
    assert_eq!(jobs.find(job.id).await.unwrap().as_ref(), Some(&job));

    job.fail("boom".into());
    jobs.update(&job).await.unwrap();

    let stored = jobs.find(job.id).await.unwrap().unwrap();
    assert_eq!(stored.status, JobStatus::Failed);
    assert_eq!(stored.error.as_deref(), Some("boom"));
    assert!(stored.completed_at.is_some());
}

#[tokio::test]
async fn list_for_user_is_scoped_and_newest_first() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let other = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool);

    let first = ResearchJob::new(user.id, "first").unwrap();
    let second = ResearchJob::new(user.id, "second").unwrap();
    let foreign = ResearchJob::new(other.id, "not mine").unwrap();
    for job in [&first, &second, &foreign] {
        jobs.insert(job).await.unwrap();
    }

    let listed = jobs.list_for_user(user.id).await.unwrap();
    let keywords: Vec<&str> = listed.iter().map(|j| j.keyword.as_str()).collect();
    assert_eq!(keywords, vec!["second", "first"]);
}

#[tokio::test]
async fn refresh_token_roundtrip_delete_and_purge() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let repo = PostgresRefreshTokenRepository::new(pool);

    let (valid, _) = RefreshToken::issue(user.id, 30);
    let (mut expired, _) = RefreshToken::issue(user.id, 30);
    expired.expires_at = Utc::now() - chrono::Duration::hours(1);
    repo.insert(&valid).await.unwrap();
    repo.insert(&expired).await.unwrap();

    // Roundtrip by hash.
    let found = repo.find_by_hash(&valid.token_hash).await.unwrap();
    assert_eq!(found, Some(valid.clone()));

    // Purge removes only the expired one.
    assert_eq!(repo.delete_expired(Utc::now()).await.unwrap(), 1);
    assert!(repo
        .find_by_hash(&expired.token_hash)
        .await
        .unwrap()
        .is_none());
    assert!(repo
        .find_by_hash(&valid.token_hash)
        .await
        .unwrap()
        .is_some());

    // Explicit delete (rotation/logout).
    repo.delete(valid.id).await.unwrap();
    assert!(repo
        .find_by_hash(&valid.token_hash)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn count_created_since_scopes_by_user_and_window() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let other = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool);

    let recent = ResearchJob::new(user.id, "recent").unwrap();
    let mut old = ResearchJob::new(user.id, "old").unwrap();
    old.created_at = Utc::now() - chrono::Duration::hours(25);
    let foreign = ResearchJob::new(other.id, "foreign").unwrap();
    for job in [&recent, &old, &foreign] {
        jobs.insert(job).await.unwrap();
    }

    let since = Utc::now() - chrono::Duration::hours(24);
    assert_eq!(jobs.count_created_since(user.id, since).await.unwrap(), 1);
}

#[tokio::test]
async fn list_unfinished_older_than_feeds_the_reaper() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool);

    let mut stale_pending = ResearchJob::new(user.id, "stale pending").unwrap();
    stale_pending.created_at = Utc::now() - chrono::Duration::hours(1);
    let mut stale_running = ResearchJob::new(user.id, "stale running").unwrap();
    stale_running.created_at = Utc::now() - chrono::Duration::hours(1);
    stale_running.start();
    let mut old_completed = ResearchJob::new(user.id, "old completed").unwrap();
    old_completed.created_at = Utc::now() - chrono::Duration::hours(1);
    old_completed.complete();
    let fresh = ResearchJob::new(user.id, "fresh").unwrap();
    for job in [&stale_pending, &stale_running, &old_completed, &fresh] {
        jobs.insert(job).await.unwrap();
    }

    let cutoff = Utc::now() - chrono::Duration::minutes(15);
    let stale = jobs.list_unfinished_older_than(cutoff).await.unwrap();
    let ids: Vec<_> = stale.iter().map(|j| j.id).collect();
    assert!(ids.contains(&stale_pending.id));
    assert!(ids.contains(&stale_running.id));
    assert!(!ids.contains(&old_completed.id));
    assert!(!ids.contains(&fresh.id));
}

#[tokio::test]
async fn results_roundtrip_with_replace_semantics() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool);
    let job = ResearchJob::new(user.id, "keyword").unwrap();
    jobs.insert(&job).await.unwrap();

    let result = |title: &str, day: Option<u32>| SearchResult {
        title: title.into(),
        url: format!("https://example.com/{title}"),
        snippet: "snippet".into(),
        published_at: day.map(|d| Utc.with_ymd_and_hms(2026, 6, d, 0, 0, 0).unwrap()),
        date_confidence: if day.is_some() {
            DateConfidence::High
        } else {
            DateConfidence::Unknown
        },
        event_type: backend::domain::EventType::Release,
        summary: Some(format!("summary of {title}")),
        raw: serde_json::json!({"source": "test"}),
    };

    jobs.store_results(job.id, &[result("stale", Some(1))])
        .await
        .unwrap();
    // Worker re-delivery replaces, never duplicates.
    jobs.store_results(
        job.id,
        &[
            result("old", Some(2)),
            result("no-date", None),
            result("new", Some(20)),
        ],
    )
    .await
    .unwrap();

    let stored = jobs.results_for(job.id).await.unwrap();
    let titles: Vec<&str> = stored.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, vec!["new", "old", "no-date"]);
    assert_eq!(stored[0].raw["source"], "test");
    // Timeline enrichment roundtrip (ADR-027).
    assert_eq!(stored[0].event_type, backend::domain::EventType::Release);
    assert_eq!(stored[0].summary.as_deref(), Some("summary of new"));
}

/// Agent mode + journal roundtrip (ADR-030): the mode survives persistence and
/// steps are idempotent on (job_id, seq), returned in order.
#[tokio::test]
async fn agent_mode_and_steps_roundtrip() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool);

    let job = ResearchJob::new(user.id, "agentic")
        .unwrap()
        .with_mode(JobMode::Agent);
    jobs.insert(&job).await.unwrap();
    let stored = jobs.find(job.id).await.unwrap().unwrap();
    assert_eq!(stored.mode, JobMode::Agent);

    let step = |seq: i32, kind: &str| AgentStep {
        seq,
        kind: kind.into(),
        detail: "agentic".into(),
        reason: "because".into(),
        new_hits: 3,
    };
    jobs.append_step(job.id, &step(1, "search")).await.unwrap();
    jobs.append_step(job.id, &step(1, "search")).await.unwrap(); // Celery retry
    jobs.append_step(job.id, &step(2, "finish")).await.unwrap();

    let steps = jobs.steps_for(job.id).await.unwrap();
    assert_eq!(
        steps
            .iter()
            .map(|s| (s.seq, s.kind.as_str(), s.new_hits))
            .collect::<Vec<_>>(),
        vec![(1, "search", 3), (2, "finish", 3)]
    );
}
