//! Single-leader coordination for the background loop (ADR-053).
//!
//! The reaper (ADR-016), the recurring-search scheduler (ADR-033) and the
//! refresh-token purge (ADR-008) run on a shared ticker in `main.rs`. With
//! several backend replicas each would run every tick — and the scheduler is
//! **not** idempotent: it would launch duplicate recurring jobs. A `LeaderLock`
//! gates the per-tick work so exactly one instance runs it. The Postgres
//! implementation (see `persistence::postgres`) uses a session advisory lock;
//! the single-instance / in-memory path always leads.

use async_trait::async_trait;

#[async_trait]
pub trait LeaderLock: Send + Sync {
    /// Try to become the leader for this tick. `true` means this instance holds
    /// the lock and should run the background work — it must then call
    /// `release`. `false` means another instance is the leader; skip the tick.
    async fn acquire(&self) -> bool;
    /// Release the lock after the tick (a no-op if this instance is not holding it).
    async fn release(&self);
}

/// Always leads — for a single-instance or in-memory deployment, where no
/// cross-replica coordination is needed.
pub struct NoopLeaderLock;

#[async_trait]
impl LeaderLock for NoopLeaderLock {
    async fn acquire(&self) -> bool {
        true
    }
    async fn release(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_lock_always_leads() {
        let lock = NoopLeaderLock;
        assert!(lock.acquire().await);
        lock.release().await; // no-op, no panic
    }
}
