use std::sync::Arc;

use uuid::Uuid;

use crate::domain::ports::{JobRepository, PortError};
use crate::domain::{sort_by_publication_date, AgentStep, ResearchJob, SearchResult};

/// Read-side use cases. Ownership is enforced here: a user can only see their own jobs.
pub struct SearchQueries {
    jobs: Arc<dyn JobRepository>,
}

impl SearchQueries {
    pub fn new(jobs: Arc<dyn JobRepository>) -> Self {
        Self { jobs }
    }

    pub async fn list(&self, user_id: Uuid) -> Result<Vec<ResearchJob>, PortError> {
        self.jobs.list_for_user(user_id).await
    }

    /// Returns the job with its results sorted by publication date (ADR-011)
    /// and its agent journal (ADR-030, empty in workflow mode), or `None` if
    /// the job does not exist or belongs to another user.
    pub async fn get(
        &self,
        user_id: Uuid,
        job_id: Uuid,
    ) -> Result<Option<(ResearchJob, Vec<SearchResult>, Vec<AgentStep>)>, PortError> {
        let Some(job) = self.jobs.find(job_id).await? else {
            return Ok(None);
        };
        if job.user_id != user_id {
            return Ok(None);
        }
        let mut results = self.jobs.results_for(job_id).await?;
        sort_by_publication_date(&mut results);
        let steps = self.jobs.steps_for(job_id).await?;
        Ok(Some((job, results, steps)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryJobRepository;

    #[tokio::test]
    async fn a_user_cannot_read_another_users_job() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let owner = Uuid::new_v4();
        let job = ResearchJob::new(owner, "secret keyword").unwrap();
        jobs.insert(&job).await.unwrap();
        let queries = SearchQueries::new(jobs);

        assert!(queries.get(owner, job.id).await.unwrap().is_some());
        assert!(queries.get(Uuid::new_v4(), job.id).await.unwrap().is_none());
    }
}
