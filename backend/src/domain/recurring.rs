//! Recurring searches (ADR-033): a saved keyword re-run on an interval by the
//! backend scheduler tick. Each run is an ordinary `ResearchJob` linked back
//! to its recurring search, so history, results and the journal need nothing
//! new. Pure domain — the scheduling itself lives in `application`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::job::{JobError, JobMode};

/// Interval bounds: floor guards against tick-speed abuse (every run costs
/// provider/LLM calls, ADR-017); ceiling is one week.
pub const MIN_INTERVAL_MINUTES: u32 = 1;
pub const MAX_INTERVAL_MINUTES: u32 = 7 * 24 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct RecurringSearch {
    pub id: Uuid,
    pub user_id: Uuid,
    pub keyword: String,
    pub mode: JobMode,
    pub interval_minutes: u32,
    pub created_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
}

impl RecurringSearch {
    pub fn new(
        user_id: Uuid,
        keyword: &str,
        mode: JobMode,
        interval_minutes: u32,
    ) -> Result<Self, JobError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(JobError::EmptyKeyword);
        }
        if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&interval_minutes) {
            return Err(JobError::InvalidInterval);
        }
        Ok(Self {
            id: Uuid::new_v4(),
            user_id,
            keyword: keyword.to_string(),
            mode,
            interval_minutes,
            created_at: super::now_utc(),
            last_run_at: None,
        })
    }

    /// Due when never run, or when the interval has elapsed since the last
    /// run. `mark_ran` is recorded even for skipped runs (quota) so a stuck
    /// user does not make the scheduler retry every tick.
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        match self.last_run_at {
            None => true,
            Some(last) => last + chrono::Duration::minutes(i64::from(self.interval_minutes)) <= now,
        }
    }

    pub fn mark_ran(&mut self, at: DateTime<Utc>) {
        self.last_run_at = Some(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_recurring_search_is_due_immediately() {
        let rs = RecurringSearch::new(Uuid::new_v4(), " rust ", JobMode::Agent, 60).unwrap();
        assert_eq!(rs.keyword, "rust");
        assert!(rs.is_due(Utc::now()));
    }

    #[test]
    fn due_again_only_after_the_interval() {
        let mut rs = RecurringSearch::new(Uuid::new_v4(), "rust", JobMode::Workflow, 60).unwrap();
        let now = Utc::now();
        rs.mark_ran(now);
        assert!(!rs.is_due(now + chrono::Duration::minutes(59)));
        assert!(rs.is_due(now + chrono::Duration::minutes(60)));
    }

    #[test]
    fn keyword_and_interval_are_validated() {
        let user = Uuid::new_v4();
        assert_eq!(
            RecurringSearch::new(user, "  ", JobMode::Workflow, 60).unwrap_err(),
            JobError::EmptyKeyword
        );
        assert_eq!(
            RecurringSearch::new(user, "k", JobMode::Workflow, 0).unwrap_err(),
            JobError::InvalidInterval
        );
        assert_eq!(
            RecurringSearch::new(user, "k", JobMode::Workflow, MAX_INTERVAL_MINUTES + 1)
                .unwrap_err(),
            JobError::InvalidInterval
        );
    }
}
