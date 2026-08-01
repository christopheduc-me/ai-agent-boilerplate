//! Recurring searches (ADR-033): a saved keyword re-run on an interval by the
//! backend scheduler tick. Each run is an ordinary `ResearchJob` linked back
//! to its recurring search, so history, results and the journal need nothing
//! new. Pure domain — the scheduling itself lives in `application`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::job::{JobError, JobMode, MAX_KEYWORD_LEN};

/// Interval bounds: floor guards against tick-speed abuse (every run costs
/// provider/LLM calls, ADR-017); ceiling is one week.
pub const MIN_INTERVAL_MINUTES: u32 = 1;
pub const MAX_INTERVAL_MINUTES: u32 = 7 * 24 * 60;

/// Webhook URL cap (ADR-056): a saved digest target is user-supplied free text
/// stored and later fetched, so bound it like the other inputs.
pub const MAX_WEBHOOK_URL_LEN: usize = 2_048;

#[derive(Debug, Clone, PartialEq)]
pub struct RecurringSearch {
    pub id: Uuid,
    pub user_id: Uuid,
    pub keyword: String,
    pub mode: JobMode,
    pub interval_minutes: u32,
    /// Digest target (ADR-036): when set, runs that deliver new results POST
    /// a digest there. None = no notification.
    pub webhook_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
}

impl RecurringSearch {
    pub fn new(
        user_id: Uuid,
        keyword: &str,
        mode: JobMode,
        interval_minutes: u32,
        webhook_url: Option<&str>,
    ) -> Result<Self, JobError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(JobError::EmptyKeyword);
        }
        if keyword.chars().count() > MAX_KEYWORD_LEN {
            return Err(JobError::KeywordTooLong);
        }
        if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&interval_minutes) {
            return Err(JobError::InvalidInterval);
        }
        let webhook_url = match webhook_url.map(str::trim).filter(|u| !u.is_empty()) {
            None => None,
            Some(url) if url.chars().count() > MAX_WEBHOOK_URL_LEN => {
                return Err(JobError::WebhookUrlTooLong)
            }
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                Some(url.to_string())
            }
            Some(_) => return Err(JobError::InvalidWebhookUrl),
        };
        Ok(Self {
            id: Uuid::new_v4(),
            user_id,
            keyword: keyword.to_string(),
            mode,
            interval_minutes,
            webhook_url,
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
        let rs = RecurringSearch::new(Uuid::new_v4(), " rust ", JobMode::Agent, 60, None).unwrap();
        assert_eq!(rs.keyword, "rust");
        assert!(rs.is_due(Utc::now()));
    }

    #[test]
    fn due_again_only_after_the_interval() {
        let mut rs =
            RecurringSearch::new(Uuid::new_v4(), "rust", JobMode::Workflow, 60, None).unwrap();
        let now = Utc::now();
        rs.mark_ran(now);
        assert!(!rs.is_due(now + chrono::Duration::minutes(59)));
        assert!(rs.is_due(now + chrono::Duration::minutes(60)));
    }

    #[test]
    fn keyword_and_interval_are_validated() {
        let user = Uuid::new_v4();
        assert_eq!(
            RecurringSearch::new(user, "  ", JobMode::Workflow, 60, None).unwrap_err(),
            JobError::EmptyKeyword
        );
        assert_eq!(
            RecurringSearch::new(user, "k", JobMode::Workflow, 0, None).unwrap_err(),
            JobError::InvalidInterval
        );
        assert_eq!(
            RecurringSearch::new(user, "k", JobMode::Workflow, MAX_INTERVAL_MINUTES + 1, None)
                .unwrap_err(),
            JobError::InvalidInterval
        );
    }

    #[test]
    fn webhook_url_is_validated_and_optional() {
        let user = Uuid::new_v4();
        let with_hook =
            RecurringSearch::new(user, "k", JobMode::Agent, 60, Some("https://ex.com/hook"))
                .unwrap();
        assert_eq!(
            with_hook.webhook_url.as_deref(),
            Some("https://ex.com/hook")
        );

        // Blank means none; anything that is not http(s) is rejected.
        let blank = RecurringSearch::new(user, "k", JobMode::Agent, 60, Some("  ")).unwrap();
        assert!(blank.webhook_url.is_none());
        assert_eq!(
            RecurringSearch::new(user, "k", JobMode::Agent, 60, Some("ftp://x")).unwrap_err(),
            JobError::InvalidWebhookUrl
        );
    }

    #[test]
    fn overlong_keyword_and_webhook_url_are_rejected() {
        // ADR-056: input caps at the domain boundary.
        let user = Uuid::new_v4();
        assert_eq!(
            RecurringSearch::new(
                user,
                &"k".repeat(MAX_KEYWORD_LEN + 1),
                JobMode::Agent,
                60,
                None
            )
            .unwrap_err(),
            JobError::KeywordTooLong
        );
        let long_url = format!("https://ex.com/{}", "a".repeat(MAX_WEBHOOK_URL_LEN));
        assert_eq!(
            RecurringSearch::new(user, "k", JobMode::Agent, 60, Some(&long_url)).unwrap_err(),
            JobError::WebhookUrlTooLong
        );
    }
}
