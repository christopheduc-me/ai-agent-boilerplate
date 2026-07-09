//! Job reaper: fails jobs stuck in pending/running past a timeout (ADR-016).
//!
//! Covers lost Celery messages, dead workers, and unreachable callbacks — the
//! cases where no `/internal/jobs/{id}/...` notification will ever arrive. A
//! late completion still overwrites the timeout failure (see `ResearchJob`).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::domain::ports::{JobRepository, PortError};

pub struct FailStaleJobs {
    jobs: Arc<dyn JobRepository>,
    timeout: Duration,
}

impl FailStaleJobs {
    pub fn new(jobs: Arc<dyn JobRepository>, timeout: Duration) -> Self {
        Self { jobs, timeout }
    }

    /// Fails every unfinished job older than the timeout; returns how many.
    pub async fn execute(&self) -> Result<u64, PortError> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(self.timeout)
                .map_err(|e| PortError(format!("invalid timeout: {e}")))?;
        let stale = self.jobs.list_unfinished_older_than(cutoff).await?;
        let mut reaped = 0;
        for mut job in stale {
            job.fail(format!(
                "timed out after {}s without a worker notification",
                self.timeout.as_secs()
            ));
            self.jobs.update(&job).await?;
            reaped += 1;
        }
        Ok(reaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryJobRepository;
    use crate::domain::ports::JobRepository;
    use crate::domain::{JobStatus, ResearchJob};
    use uuid::Uuid;

    fn job_created_secs_ago(secs: i64) -> ResearchJob {
        let mut job = ResearchJob::new(Uuid::new_v4(), "keyword").unwrap();
        job.created_at = Utc::now() - chrono::Duration::seconds(secs);
        job
    }

    #[tokio::test]
    async fn fails_stale_pending_and_running_jobs_only() {
        let jobs = Arc::new(InMemoryJobRepository::default());

        let stale_pending = job_created_secs_ago(3600);
        let mut stale_running = job_created_secs_ago(3600);
        stale_running.start();
        let mut old_completed = job_created_secs_ago(3600);
        old_completed.complete();
        let fresh_pending = job_created_secs_ago(10);
        for job in [
            &stale_pending,
            &stale_running,
            &old_completed,
            &fresh_pending,
        ] {
            jobs.insert(job).await.unwrap();
        }

        let reaper = FailStaleJobs::new(jobs.clone(), Duration::from_secs(900));
        assert_eq!(reaper.execute().await.unwrap(), 2);

        for id in [stale_pending.id, stale_running.id] {
            let stored = jobs.find(id).await.unwrap().unwrap();
            assert_eq!(stored.status, JobStatus::Failed);
            assert!(stored.error.as_deref().unwrap().contains("timed out"));
        }
        let completed = jobs.find(old_completed.id).await.unwrap().unwrap();
        assert_eq!(completed.status, JobStatus::Completed);
        let fresh = jobs.find(fresh_pending.id).await.unwrap().unwrap();
        assert_eq!(fresh.status, JobStatus::Pending);
    }

    #[tokio::test]
    async fn nothing_to_reap_returns_zero() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        jobs.insert(&job_created_secs_ago(10)).await.unwrap();

        let reaper = FailStaleJobs::new(jobs, Duration::from_secs(900));
        assert_eq!(reaper.execute().await.unwrap(), 0);
    }
}
