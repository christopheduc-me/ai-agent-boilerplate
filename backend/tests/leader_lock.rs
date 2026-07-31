//! Integration test for the Postgres leader lock (ADR-053).
//!
//! Runs against `DATABASE_URL` (the compose `postgres` service locally, a CI
//! service in the pipeline); skipped without it so `cargo test` stays offline.
//! Uses a unique advisory key per run, so it never contends with a running
//! backend instance or with parallel tests.

use backend::adapters::leader_lock::LeaderLock;
use backend::adapters::persistence::postgres::PostgresLeaderLock;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping leader-lock test: DATABASE_URL not set");
        return None;
    };
    Some(
        PgPool::connect(&url)
            .await
            .expect("cannot connect to DATABASE_URL"),
    )
}

fn unique_key() -> i64 {
    // A random key isolates this run from the production key and other tests.
    i64::from(uuid::Uuid::new_v4().as_u128() as u32)
}

#[tokio::test]
async fn only_one_instance_leads_at_a_time() {
    let Some(pool) = pool().await else { return };
    let key = unique_key();
    let first = PostgresLeaderLock::with_key(pool.clone(), key);
    let second = PostgresLeaderLock::with_key(pool.clone(), key);

    // The first instance becomes the leader; the second is locked out.
    assert!(first.acquire().await, "first instance should lead");
    assert!(
        !second.acquire().await,
        "second instance must not lead while the first holds the lock"
    );

    // After the leader releases, another instance can take over.
    first.release().await;
    assert!(
        second.acquire().await,
        "after release, another instance can lead"
    );
    second.release().await;
}
