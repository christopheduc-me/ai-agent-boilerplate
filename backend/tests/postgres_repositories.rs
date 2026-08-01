//! Integration tests for the PostgreSQL adapter (ADR-007).
//!
//! They run against the database pointed to by `DATABASE_URL` — the compose
//! `postgres` service locally, a GitLab CI service in the pipeline (ADR-012).
//! Without `DATABASE_URL` they are skipped so `cargo test` stays usable offline.
//! Each test works on freshly generated UUIDs, so tests are isolated and can
//! run in parallel against a shared database.

use backend::adapters::persistence::postgres::{
    run_migrations, PostgresJobRepository, PostgresRecurringSearchRepository,
    PostgresRefreshTokenRepository, PostgresSecurityAudit, PostgresUserRepository,
};
use backend::domain::ports::{
    JobRepository, RecurringSearchRepository, RefreshTokenRepository, SecurityAudit, UserRepository,
};
use backend::domain::{
    AgentStep, DateConfidence, JobMode, JobStatus, RecurringSearch, RefreshToken, ResearchJob,
    SearchResult, SecurityEvent, SecurityEventKind, User,
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

    // Reuse detection (ADR-056): mark_consumed keeps the row but flags it.
    let at = Utc::now();
    repo.mark_consumed(valid.id, at).await.unwrap();
    let consumed = repo.find_by_hash(&valid.token_hash).await.unwrap().unwrap();
    assert!(consumed.is_consumed());
    assert_eq!(consumed.consumed_at.unwrap().timestamp(), at.timestamp());

    // Explicit delete (logout of a single row).
    repo.delete(valid.id).await.unwrap();
    assert!(repo
        .find_by_hash(&valid.token_hash)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn security_events_record_list_newest_first_and_purge() {
    // ADR-057: append, read back newest-first, and retention purge.
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let audit = PostgresSecurityAudit::new(pool);

    // Clean slate: this is the only test writing security_events, and
    // list_recent is a global operator view, so wipe first for a deterministic
    // count (the shared DB persists rows between runs).
    audit
        .delete_before(Utc::now() + chrono::Duration::days(1))
        .await
        .unwrap();

    let mut old = SecurityEvent::new(
        SecurityEventKind::LoginFailed,
        None,
        Some("1.2.3.4".into()),
        "old",
    );
    old.created_at = Utc::now() - chrono::Duration::days(120);
    let recent = SecurityEvent::new(
        SecurityEventKind::RefreshReuseDetected,
        Some(user.id),
        None,
        "recent",
    );
    audit.record(&old).await.unwrap();
    audit.record(&recent).await.unwrap();

    // Newest first.
    let listed = audit.list_recent(10).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].detail, "recent");
    assert_eq!(listed[0].user_id, Some(user.id));
    assert_eq!(listed[1].client_ip.as_deref(), Some("1.2.3.4"));

    // Retention purge drops only the old one.
    let cutoff = Utc::now() - chrono::Duration::days(90);
    assert_eq!(audit.delete_before(cutoff).await.unwrap(), 1);
    let remaining = audit.list_recent(10).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].detail, "recent");
}

#[tokio::test]
async fn delete_family_revokes_a_whole_rotation_lineage() {
    // ADR-056: reuse detection revokes every token sharing a family_id.
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let repo = PostgresRefreshTokenRepository::new(pool);

    let (root, _) = RefreshToken::issue(user.id, 30);
    let (child, _) = RefreshToken::issue_in_family(user.id, root.family_id, 30);
    let (other, _) = RefreshToken::issue(user.id, 30); // a different family
    repo.insert(&root).await.unwrap();
    repo.insert(&child).await.unwrap();
    repo.insert(&other).await.unwrap();

    repo.delete_family(root.family_id).await.unwrap();

    assert!(repo.find_by_hash(&root.token_hash).await.unwrap().is_none());
    assert!(repo
        .find_by_hash(&child.token_hash)
        .await
        .unwrap()
        .is_none());
    // The unrelated family is untouched.
    assert!(repo
        .find_by_hash(&other.token_hash)
        .await
        .unwrap()
        .is_some());
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
        is_new: true,
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

#[tokio::test]
async fn recurring_search_roundtrip_due_and_memory() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let recurring_repo = PostgresRecurringSearchRepository::new(pool.clone());
    let jobs = PostgresJobRepository::new(pool.clone());

    let rs = RecurringSearch::new(user.id, "rust releases", JobMode::Agent, 60, None).unwrap();
    recurring_repo.insert(&rs).await.unwrap();

    // Roundtrip + due immediately (never run).
    let listed = recurring_repo.list_for_user(user.id).await.unwrap();
    assert_eq!(listed, vec![rs.clone()]);
    assert!(recurring_repo
        .list_due(Utc::now())
        .await
        .unwrap()
        .iter()
        .any(|s| s.id == rs.id));

    // After a run: not due until the interval elapses.
    recurring_repo.mark_ran(rs.id, Utc::now()).await.unwrap();
    assert!(!recurring_repo
        .list_due(Utc::now())
        .await
        .unwrap()
        .iter()
        .any(|s| s.id == rs.id));

    // A linked run delivers results: they become the memory of the next run.
    let job = ResearchJob::new(user.id, "rust releases")
        .unwrap()
        .with_mode(JobMode::Agent)
        .with_recurring(rs.id);
    jobs.insert(&job).await.unwrap();
    let result = |url: &str| SearchResult {
        title: "t".into(),
        url: url.into(),
        snippet: "s".into(),
        published_at: None,
        date_confidence: DateConfidence::Unknown,
        event_type: backend::domain::EventType::default(),
        summary: None,
        is_new: true,
        raw: serde_json::Value::Null,
    };
    jobs.store_results(job.id, &[result("https://a"), result("https://b")])
        .await
        .unwrap();
    let mut urls = jobs.recent_urls_for_recurring(rs.id, 200).await.unwrap();
    urls.sort();
    assert_eq!(urls, vec!["https://a".to_string(), "https://b".to_string()]);
    // The stored job kept its recurring link and the is_new flag roundtrips.
    let stored = jobs.find(job.id).await.unwrap().unwrap();
    assert_eq!(stored.recurring_search_id, Some(rs.id));
    assert!(jobs.results_for(job.id).await.unwrap()[0].is_new);

    // Ownership guard on delete.
    assert!(!recurring_repo.delete(Uuid::new_v4(), rs.id).await.unwrap());
    assert!(recurring_repo.delete(user.id, rs.id).await.unwrap());
    // History survives the deletion (ON DELETE SET NULL).
    let kept = jobs.find(job.id).await.unwrap().unwrap();
    assert_eq!(kept.recurring_search_id, None);
}

#[tokio::test]
async fn usage_accumulates_and_survives_lifecycle_updates() {
    let Some(pool) = pool().await else { return };
    let user = insert_user(&pool).await;
    let jobs = PostgresJobRepository::new(pool.clone());
    let mut job = ResearchJob::new(user.id, "usage").unwrap();
    jobs.insert(&job).await.unwrap();

    let attempt = backend::domain::JobUsage {
        llm_calls: 3,
        llm_input_tokens: 1000,
        llm_output_tokens: 200,
        search_calls: 1,
        cost_usd: 0.02,
    };
    jobs.add_usage(job.id, &attempt).await.unwrap();
    jobs.add_usage(job.id, &attempt).await.unwrap(); // second attempt adds

    // A lifecycle update (completion) must not clobber the accumulated spend.
    job.complete();
    jobs.update(&job).await.unwrap();

    let stored = jobs.find(job.id).await.unwrap().unwrap();
    assert_eq!(stored.usage.llm_calls, 6);
    assert_eq!(stored.usage.llm_input_tokens, 2000);
    assert_eq!(stored.usage.search_calls, 2);
    assert!((stored.usage.cost_usd - 0.04).abs() < 1e-9);
}
