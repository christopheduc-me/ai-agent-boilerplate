use std::sync::Arc;

use uuid::Uuid;

use crate::domain::ports::{JobRepository, PortError};
use crate::domain::SearchResult;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("job not found")]
    JobNotFound,
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

/// Receives the agent's output (HTTP callback from the worker, see ADR-006).
pub struct IngestResults {
    jobs: Arc<dyn JobRepository>,
}

impl IngestResults {
    pub fn new(jobs: Arc<dyn JobRepository>) -> Self {
        Self { jobs }
    }

    /// Worker picked the job up (POST /internal/jobs/{id}/started).
    pub async fn start(&self, job_id: Uuid) -> Result<(), IngestError> {
        let mut job = self
            .jobs
            .find(job_id)
            .await?
            .ok_or(IngestError::JobNotFound)?;
        job.start();
        self.jobs.update(&job).await?;
        Ok(())
    }

    pub async fn complete(
        &self,
        job_id: Uuid,
        results: &[SearchResult],
    ) -> Result<(), IngestError> {
        let mut job = self
            .jobs
            .find(job_id)
            .await?
            .ok_or(IngestError::JobNotFound)?;
        self.jobs.store_results(job_id, results).await?;
        job.complete();
        self.jobs.update(&job).await?;
        Ok(())
    }

    pub async fn fail(&self, job_id: Uuid, error: String) -> Result<(), IngestError> {
        let mut job = self
            .jobs
            .find(job_id)
            .await?
            .ok_or(IngestError::JobNotFound)?;
        job.fail(error);
        self.jobs.update(&job).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryJobRepository;
    use crate::domain::{DateConfidence, JobStatus, ResearchJob};

    fn a_result(title: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: "https://example.com".into(),
            snippet: "...".into(),
            published_at: None,
            date_confidence: DateConfidence::Unknown,
            event_type: crate::domain::EventType::default(),
            summary: None,
            raw: serde_json::Value::Null,
        }
    }

    async fn repo_with_job() -> (Arc<InMemoryJobRepository>, ResearchJob) {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let job = ResearchJob::new(Uuid::new_v4(), "keyword").unwrap();
        jobs.insert(&job).await.unwrap();
        (jobs, job)
    }

    #[tokio::test]
    async fn start_marks_the_job_running() {
        let (jobs, job) = repo_with_job().await;
        let ingest = IngestResults::new(jobs.clone());

        ingest.start(job.id).await.unwrap();

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Running);
    }

    #[tokio::test]
    async fn late_start_does_not_reopen_a_finished_job() {
        let (jobs, job) = repo_with_job().await;
        let ingest = IngestResults::new(jobs.clone());

        ingest.complete(job.id, &[a_result("r")]).await.unwrap();
        ingest.start(job.id).await.unwrap(); // Celery retry after completion

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Completed);
    }

    #[tokio::test]
    async fn stores_results_and_completes_the_job() {
        let (jobs, job) = repo_with_job().await;
        let ingest = IngestResults::new(jobs.clone());

        ingest
            .complete(job.id, &[a_result("r1"), a_result("r2")])
            .await
            .unwrap();

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Completed);
        assert_eq!(jobs.results_for(job.id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn records_agent_failure() {
        let (jobs, job) = repo_with_job().await;
        let ingest = IngestResults::new(jobs.clone());

        ingest
            .fail(job.id, "provider quota exceeded".into())
            .await
            .unwrap();

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Failed);
        assert_eq!(stored.error.as_deref(), Some("provider quota exceeded"));
    }

    #[tokio::test]
    async fn unknown_job_is_an_error() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let ingest = IngestResults::new(jobs);

        let err = ingest.complete(Uuid::new_v4(), &[]).await.unwrap_err();
        assert!(matches!(err, IngestError::JobNotFound));
    }
}
