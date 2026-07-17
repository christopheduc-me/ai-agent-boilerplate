use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    /// The agent asked the user a clarification question (ADR-032): the job is
    /// paused — not stuck, so the reaper leaves it alone — until the answer
    /// arrives and re-dispatches it.
    #[serde(rename = "awaiting_input")]
    AwaitingInput,
    Completed,
    Failed,
}

/// How the research runs (ADR-030): the fixed pipeline, or the agentic loop
/// where the LLM policy decides the queries and when to stop. The default
/// keeps pre-ADR-030 clients and payloads working unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum JobMode {
    #[default]
    Workflow,
    Agent,
}

/// One decision of the agentic loop (ADR-030), recorded for the live journal.
/// `kind` stays an open string ("search" / "finish" today) so newer agents can
/// introduce step kinds without breaking older backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStep {
    pub seq: i32,
    pub kind: String,
    pub detail: String,
    pub reason: String,
    #[serde(default)]
    pub new_hits: i32,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum JobError {
    #[error("keyword must not be empty")]
    EmptyKeyword,
    #[error("interval must be between 1 minute and 7 days")]
    InvalidInterval,
    #[error("webhook url must start with http:// or https://")]
    InvalidWebhookUrl,
    #[error("question must not be empty")]
    EmptyQuestion,
    #[error("answer must not be empty")]
    EmptyAnswer,
    #[error("job is not awaiting input")]
    NotAwaitingInput,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResearchJob {
    pub id: Uuid,
    pub user_id: Uuid,
    pub keyword: String,
    pub mode: JobMode,
    pub status: JobStatus,
    pub error: Option<String>,
    /// Clarification dialog (ADR-032): the agent's question and, once the
    /// user replied, the answer forwarded back to the agent on re-dispatch.
    pub question: Option<String>,
    pub answer: Option<String>,
    /// Set when the job was launched by the scheduler for a recurring search
    /// (ADR-033); one-shot searches leave it null.
    pub recurring_search_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl ResearchJob {
    pub fn new(user_id: Uuid, keyword: &str) -> Result<Self, JobError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(JobError::EmptyKeyword);
        }
        Ok(Self {
            id: Uuid::new_v4(),
            user_id,
            keyword: keyword.to_string(),
            mode: JobMode::default(),
            status: JobStatus::Pending,
            error: None,
            question: None,
            answer: None,
            recurring_search_id: None,
            created_at: super::now_utc(),
            completed_at: None,
        })
    }

    pub fn with_mode(mut self, mode: JobMode) -> Self {
        self.mode = mode;
        self
    }

    /// Links a scheduler-launched run to its recurring search (ADR-033).
    pub fn with_recurring(mut self, recurring_search_id: Uuid) -> Self {
        self.recurring_search_id = Some(recurring_search_id);
        self
    }

    /// Worker picked the job up. Only a pending job transitions; anything else
    /// is a no-op so retried/out-of-order notifications stay harmless.
    pub fn start(&mut self) {
        if self.status == JobStatus::Pending {
            self.status = JobStatus::Running;
        }
    }

    /// Completing always wins: results arriving after a timeout-failure are
    /// still valuable, so a late completion overwrites `Failed`.
    pub fn complete(&mut self) {
        self.status = JobStatus::Completed;
        self.error = None;
        self.completed_at = Some(super::now_utc());
    }

    /// A failure never clobbers a completed job (late duplicate callbacks).
    pub fn fail(&mut self, error: String) {
        if self.status == JobStatus::Completed {
            return;
        }
        self.status = JobStatus::Failed;
        self.error = Some(error);
        self.completed_at = Some(super::now_utc());
    }

    /// The agent asked a clarification question (ADR-032). Only a running job
    /// pauses; a repeat of the same notification (Celery retry) is a no-op,
    /// and a question never reopens a finished job.
    pub fn request_input(&mut self, question: &str) -> Result<(), JobError> {
        let question = question.trim();
        if question.is_empty() {
            return Err(JobError::EmptyQuestion);
        }
        if self.status == JobStatus::Running || self.status == JobStatus::Pending {
            self.status = JobStatus::AwaitingInput;
            self.question = Some(question.to_string());
        }
        Ok(())
    }

    /// The user answered (ADR-032): the job goes back to `pending` for
    /// re-dispatch, carrying the answer as the clarification.
    pub fn provide_answer(&mut self, answer: &str) -> Result<(), JobError> {
        let answer = answer.trim();
        if answer.is_empty() {
            return Err(JobError::EmptyAnswer);
        }
        if self.status != JobStatus::AwaitingInput {
            return Err(JobError::NotAwaitingInput);
        }
        self.answer = Some(answer.to_string());
        self.status = JobStatus::Pending;
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.status, JobStatus::Completed | JobStatus::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_job_starts_pending_with_trimmed_keyword() {
        let job = ResearchJob::new(Uuid::new_v4(), "  rust async  ").unwrap();
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.keyword, "rust async");
        assert!(job.error.is_none());
        assert!(job.completed_at.is_none());
    }

    #[test]
    fn empty_keyword_is_rejected() {
        let err = ResearchJob::new(Uuid::new_v4(), "   ").unwrap_err();
        assert_eq!(err, JobError::EmptyKeyword);
    }

    #[test]
    fn complete_sets_status_and_timestamp() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.complete();
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn fail_records_the_error() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.fail("boom".into());
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("boom"));
    }

    #[test]
    fn start_transitions_only_from_pending() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.start();
        assert_eq!(job.status, JobStatus::Running);

        job.complete();
        job.start(); // late/duplicate notification is a no-op
        assert_eq!(job.status, JobStatus::Completed);
    }

    #[test]
    fn late_completion_overwrites_a_timeout_failure() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.fail("timed out".into());
        job.complete();
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.error.is_none());
    }

    #[test]
    fn failure_never_clobbers_a_completed_job() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.complete();
        job.fail("late duplicate".into());
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.error.is_none());
    }

    #[test]
    fn request_input_pauses_a_running_job_idempotently() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.start();
        job.request_input("Animal or car?").unwrap();
        assert_eq!(job.status, JobStatus::AwaitingInput);
        assert_eq!(job.question.as_deref(), Some("Animal or car?"));

        job.request_input("Animal or car?").unwrap(); // Celery retry
        assert_eq!(job.status, JobStatus::AwaitingInput);
    }

    #[test]
    fn request_input_never_reopens_a_finished_job() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.complete();
        job.request_input("late question").unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.question.is_none());
    }

    #[test]
    fn empty_question_is_rejected() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.start();
        assert_eq!(job.request_input("  "), Err(JobError::EmptyQuestion));
    }

    #[test]
    fn provide_answer_requeues_the_job_with_the_answer() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.start();
        job.request_input("Animal or car?").unwrap();

        job.provide_answer(" the car ").unwrap();

        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.answer.as_deref(), Some("the car"));
        assert_eq!(job.question.as_deref(), Some("Animal or car?"));
    }

    #[test]
    fn provide_answer_requires_the_awaiting_state_and_a_non_empty_answer() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        assert_eq!(job.provide_answer("cars"), Err(JobError::NotAwaitingInput));
        job.start();
        job.request_input("q?").unwrap();
        assert_eq!(job.provide_answer("   "), Err(JobError::EmptyAnswer));
    }

    #[test]
    fn awaiting_input_is_not_a_terminal_state() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        job.start();
        job.request_input("q?").unwrap();
        assert!(!job.is_finished());
    }

    #[test]
    fn is_finished_matches_terminal_states() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        assert!(!job.is_finished());
        job.start();
        assert!(!job.is_finished());
        job.complete();
        assert!(job.is_finished());
    }
}
