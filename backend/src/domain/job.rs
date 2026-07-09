use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum JobError {
    #[error("keyword must not be empty")]
    EmptyKeyword,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResearchJob {
    pub id: Uuid,
    pub user_id: Uuid,
    pub keyword: String,
    pub status: JobStatus,
    pub error: Option<String>,
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
            status: JobStatus::Pending,
            error: None,
            created_at: Utc::now(),
            completed_at: None,
        })
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
        self.completed_at = Some(Utc::now());
    }

    /// A failure never clobbers a completed job (late duplicate callbacks).
    pub fn fail(&mut self, error: String) {
        if self.status == JobStatus::Completed {
            return;
        }
        self.status = JobStatus::Failed;
        self.error = Some(error);
        self.completed_at = Some(Utc::now());
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
    fn is_finished_matches_terminal_states() {
        let mut job = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        assert!(!job.is_finished());
        job.start();
        assert!(!job.is_finished());
        job.complete();
        assert!(job.is_finished());
    }
}
