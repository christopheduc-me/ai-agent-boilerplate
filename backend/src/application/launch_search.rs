use std::sync::Arc;

use uuid::Uuid;

use crate::domain::job::JobError;
use crate::domain::ports::{JobDispatcher, JobRepository, PortError};
use crate::domain::{JobMode, ResearchJob};

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(transparent)]
    InvalidJob(#[from] JobError),
    #[error("daily search quota reached ({0} searches per 24h)")]
    QuotaExceeded(u32),
    #[error("failed to dispatch job to the agent")]
    DispatchFailed,
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

pub struct LaunchSearch {
    jobs: Arc<dyn JobRepository>,
    dispatcher: Arc<dyn JobDispatcher>,
    /// Max searches per user per rolling 24h — every search costs Tavily and
    /// Anthropic API calls, so this caps the spend a single account can cause
    /// (ADR-017).
    daily_quota: u32,
}

impl LaunchSearch {
    pub fn new(
        jobs: Arc<dyn JobRepository>,
        dispatcher: Arc<dyn JobDispatcher>,
        daily_quota: u32,
    ) -> Self {
        Self {
            jobs,
            dispatcher,
            daily_quota,
        }
    }

    /// Checks the quota, persists the job (status `pending`), then hands it to
    /// the agent. If dispatching fails, the job is kept and marked `failed` so
    /// the user gets feedback instead of a silently lost search.
    pub async fn execute(
        &self,
        user_id: Uuid,
        keyword: &str,
        mode: JobMode,
    ) -> Result<ResearchJob, LaunchError> {
        let since = chrono::Utc::now() - chrono::Duration::hours(24);
        let used = self.jobs.count_created_since(user_id, since).await?;
        if used >= u64::from(self.daily_quota) {
            return Err(LaunchError::QuotaExceeded(self.daily_quota));
        }

        let mut job = ResearchJob::new(user_id, keyword)?.with_mode(mode);
        self.jobs.insert(&job).await?;

        if let Err(err) = self.dispatcher.dispatch(&job, &[]).await {
            job.fail(format!("dispatch failed: {err}"));
            self.jobs.update(&job).await?;
            return Err(LaunchError::DispatchFailed);
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryJobRepository;
    use crate::domain::JobStatus;
    use async_trait::async_trait;
    use std::sync::Mutex;

    const TEST_QUOTA: u32 = 20;

    struct RecordingDispatcher {
        dispatched: Mutex<Vec<Uuid>>,
        fail: bool,
    }

    impl RecordingDispatcher {
        fn ok() -> Self {
            Self {
                dispatched: Mutex::new(vec![]),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                dispatched: Mutex::new(vec![]),
                fail: true,
            }
        }
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: &ResearchJob, _seen: &[String]) -> Result<(), PortError> {
            if self.fail {
                return Err(PortError("agent unreachable".into()));
            }
            self.dispatched.lock().unwrap().push(job.id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn persists_then_dispatches_the_job() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let dispatcher = Arc::new(RecordingDispatcher::ok());
        let launch = LaunchSearch::new(jobs.clone(), dispatcher.clone(), TEST_QUOTA);

        let job = launch
            .execute(Uuid::new_v4(), "rust axum", JobMode::Workflow)
            .await
            .unwrap();

        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(*dispatcher.dispatched.lock().unwrap(), vec![job.id]);
        assert_eq!(jobs.find(job.id).await.unwrap(), Some(job));
    }

    #[tokio::test]
    async fn marks_the_job_failed_when_dispatch_fails() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let launch = LaunchSearch::new(
            jobs.clone(),
            Arc::new(RecordingDispatcher::failing()),
            TEST_QUOTA,
        );
        let user_id = Uuid::new_v4();

        let err = launch
            .execute(user_id, "rust axum", JobMode::Workflow)
            .await
            .unwrap_err();

        assert!(matches!(err, LaunchError::DispatchFailed));
        let stored = jobs.list_for_user(user_id).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].status, JobStatus::Failed);
        assert!(stored[0]
            .error
            .as_deref()
            .unwrap()
            .contains("dispatch failed"));
    }

    #[tokio::test]
    async fn rejects_empty_keyword_without_persisting() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let launch = LaunchSearch::new(
            jobs.clone(),
            Arc::new(RecordingDispatcher::ok()),
            TEST_QUOTA,
        );
        let user_id = Uuid::new_v4();

        let err = launch
            .execute(user_id, "  ", JobMode::Workflow)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            LaunchError::InvalidJob(JobError::EmptyKeyword)
        ));
        assert!(jobs.list_for_user(user_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn enforces_the_daily_quota_per_user() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let launch = LaunchSearch::new(jobs.clone(), Arc::new(RecordingDispatcher::ok()), 2);
        let user_id = Uuid::new_v4();

        launch
            .execute(user_id, "one", JobMode::Workflow)
            .await
            .unwrap();
        launch
            .execute(user_id, "two", JobMode::Workflow)
            .await
            .unwrap();
        let err = launch
            .execute(user_id, "three", JobMode::Workflow)
            .await
            .unwrap_err();

        assert!(matches!(err, LaunchError::QuotaExceeded(2)));
        assert_eq!(jobs.list_for_user(user_id).await.unwrap().len(), 2);

        // The quota is per user: another account is unaffected.
        launch
            .execute(Uuid::new_v4(), "other", JobMode::Workflow)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn quota_only_counts_the_last_24_hours() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let user_id = Uuid::new_v4();
        let mut old_job = ResearchJob::new(user_id, "yesterday").unwrap();
        old_job.created_at = chrono::Utc::now() - chrono::Duration::hours(25);
        jobs.insert(&old_job).await.unwrap();

        let launch = LaunchSearch::new(jobs, Arc::new(RecordingDispatcher::ok()), 1);
        launch
            .execute(user_id, "today", JobMode::Workflow)
            .await
            .unwrap();
    }
}
