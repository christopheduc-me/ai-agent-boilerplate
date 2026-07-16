//! Human-in-the-loop (ADR-032): the user answers the agent's clarification
//! question and the job goes back through the dispatch pipeline, this time
//! carrying the answer.

use std::sync::Arc;

use uuid::Uuid;

use crate::domain::job::JobError;
use crate::domain::ports::{JobDispatcher, JobRepository, PortError};
use crate::domain::ResearchJob;

#[derive(Debug, thiserror::Error)]
pub enum AnswerError {
    #[error("search not found")]
    NotFound,
    #[error(transparent)]
    InvalidAnswer(#[from] JobError),
    #[error("failed to dispatch job to the agent")]
    DispatchFailed,
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

pub struct AnswerClarification {
    jobs: Arc<dyn JobRepository>,
    dispatcher: Arc<dyn JobDispatcher>,
}

impl AnswerClarification {
    pub fn new(jobs: Arc<dyn JobRepository>, dispatcher: Arc<dyn JobDispatcher>) -> Self {
        Self { jobs, dispatcher }
    }

    /// Stores the answer, clears the previous journal (the resumed loop starts
    /// fresh — replace semantics, ADR-016), and re-dispatches. Like the launch
    /// path, a dispatch failure marks the job failed so the user sees it.
    pub async fn execute(
        &self,
        user_id: Uuid,
        job_id: Uuid,
        answer: &str,
    ) -> Result<ResearchJob, AnswerError> {
        let mut job = self
            .jobs
            .find(job_id)
            .await?
            .filter(|job| job.user_id == user_id)
            .ok_or(AnswerError::NotFound)?;
        job.provide_answer(answer)?;
        self.jobs.clear_steps(job_id).await?;
        self.jobs.update(&job).await?;

        // A recurring run keeps its memory across the pause (ADR-033).
        let seen_urls = match job.recurring_search_id {
            Some(rs_id) => self.jobs.recent_urls_for_recurring(rs_id, 200).await?,
            None => Vec::new(),
        };
        if let Err(err) = self.dispatcher.dispatch(&job, &seen_urls).await {
            job.fail(format!("dispatch failed: {err}"));
            self.jobs.update(&job).await?;
            return Err(AnswerError::DispatchFailed);
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryJobRepository;
    use crate::domain::ports::JobRepository;
    use crate::domain::{AgentStep, JobStatus};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct RecordingDispatcher {
        dispatched: Mutex<Vec<ResearchJob>>,
        fail: bool,
    }

    impl RecordingDispatcher {
        fn ok() -> Self {
            Self {
                dispatched: Mutex::new(vec![]),
                fail: false,
            }
        }
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: &ResearchJob, _seen: &[String]) -> Result<(), PortError> {
            if self.fail {
                return Err(PortError("agent unreachable".into()));
            }
            self.dispatched.lock().unwrap().push(job.clone());
            Ok(())
        }
    }

    async fn awaiting_job(jobs: &InMemoryJobRepository) -> ResearchJob {
        let mut job = ResearchJob::new(Uuid::new_v4(), "jaguar").unwrap();
        job.start();
        job.request_input("The animal or the car?").unwrap();
        jobs.insert(&job).await.unwrap();
        jobs.append_step(
            job.id,
            &AgentStep {
                seq: 1,
                kind: "search".into(),
                detail: "jaguar".into(),
                reason: "r".into(),
                new_hits: 2,
            },
        )
        .await
        .unwrap();
        job
    }

    #[tokio::test]
    async fn answer_requeues_clears_the_journal_and_redispatches() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let job = awaiting_job(&jobs).await;
        let dispatcher = Arc::new(RecordingDispatcher::ok());
        let answer = AnswerClarification::new(jobs.clone(), dispatcher.clone());

        let updated = answer
            .execute(job.user_id, job.id, "the car")
            .await
            .unwrap();

        assert_eq!(updated.status, JobStatus::Pending);
        assert_eq!(updated.answer.as_deref(), Some("the car"));
        // The resumed loop writes a fresh journal (replace semantics).
        assert!(jobs.steps_for(job.id).await.unwrap().is_empty());
        // The dispatched job carries the answer for the agent.
        let dispatched = dispatcher.dispatched.lock().unwrap();
        assert_eq!(dispatched[0].answer.as_deref(), Some("the car"));
    }

    #[tokio::test]
    async fn foreign_or_unknown_jobs_are_not_found() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let job = awaiting_job(&jobs).await;
        let answer = AnswerClarification::new(jobs, Arc::new(RecordingDispatcher::ok()));

        let err = answer
            .execute(Uuid::new_v4(), job.id, "the car")
            .await
            .unwrap_err();
        assert!(matches!(err, AnswerError::NotFound));
    }

    #[tokio::test]
    async fn a_job_that_is_not_awaiting_rejects_the_answer() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        jobs.insert(&job).await.unwrap();
        let answer = AnswerClarification::new(jobs, Arc::new(RecordingDispatcher::ok()));

        let err = answer
            .execute(job.user_id, job.id, "hello")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AnswerError::InvalidAnswer(JobError::NotAwaitingInput)
        ));
    }

    #[tokio::test]
    async fn dispatch_failure_marks_the_job_failed() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let job = awaiting_job(&jobs).await;
        let dispatcher = Arc::new(RecordingDispatcher {
            dispatched: Mutex::new(vec![]),
            fail: true,
        });
        let answer = AnswerClarification::new(jobs.clone(), dispatcher);

        let err = answer
            .execute(job.user_id, job.id, "the car")
            .await
            .unwrap_err();

        assert!(matches!(err, AnswerError::DispatchFailed));
        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Failed);
    }
}
