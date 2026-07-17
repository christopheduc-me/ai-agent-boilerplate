//! Recurring searches CRUD (ADR-033). The scheduling itself lives in
//! `run_due_searches`; here is only what the user manages.

use std::sync::Arc;

use uuid::Uuid;

use crate::domain::job::JobError;
use crate::domain::ports::{PortError, RecurringSearchRepository};
use crate::domain::{JobMode, RecurringSearch};

#[derive(Debug, thiserror::Error)]
pub enum RecurringError {
    #[error(transparent)]
    Invalid(#[from] JobError),
    #[error("recurring search not found")]
    NotFound,
    #[error("too many recurring searches ({0} max)")]
    TooMany(usize),
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

/// A user cannot hoard schedules: each run also counts against the daily
/// search quota (ADR-017), this cap only keeps the scheduler scan bounded.
const MAX_PER_USER: usize = 20;

pub struct RecurringSearches {
    repo: Arc<dyn RecurringSearchRepository>,
}

impl RecurringSearches {
    pub fn new(repo: Arc<dyn RecurringSearchRepository>) -> Self {
        Self { repo }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        keyword: &str,
        mode: JobMode,
        interval_minutes: u32,
        webhook_url: Option<&str>,
    ) -> Result<RecurringSearch, RecurringError> {
        if self.repo.list_for_user(user_id).await?.len() >= MAX_PER_USER {
            return Err(RecurringError::TooMany(MAX_PER_USER));
        }
        let search = RecurringSearch::new(user_id, keyword, mode, interval_minutes, webhook_url)?;
        self.repo.insert(&search).await?;
        Ok(search)
    }

    pub async fn list(&self, user_id: Uuid) -> Result<Vec<RecurringSearch>, RecurringError> {
        Ok(self.repo.list_for_user(user_id).await?)
    }

    pub async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<(), RecurringError> {
        if self.repo.delete(user_id, id).await? {
            Ok(())
        } else {
            Err(RecurringError::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryRecurringSearchRepository;

    fn service() -> RecurringSearches {
        RecurringSearches::new(Arc::new(InMemoryRecurringSearchRepository::default()))
    }

    #[tokio::test]
    async fn create_list_delete_roundtrip() {
        let service = service();
        let user = Uuid::new_v4();

        let created = service
            .create(user, "rust releases", JobMode::Agent, 60, None)
            .await
            .unwrap();
        assert_eq!(service.list(user).await.unwrap(), vec![created.clone()]);

        service.delete(user, created.id).await.unwrap();
        assert!(service.list(user).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_foreign_search_is_not_found() {
        let service = service();
        let owner = Uuid::new_v4();
        let created = service
            .create(owner, "k", JobMode::Workflow, 60, None)
            .await
            .unwrap();

        let err = service
            .delete(Uuid::new_v4(), created.id)
            .await
            .unwrap_err();
        assert!(matches!(err, RecurringError::NotFound));
        assert_eq!(service.list(owner).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn validation_and_cap_are_enforced() {
        let service = service();
        let user = Uuid::new_v4();
        assert!(matches!(
            service.create(user, " ", JobMode::Workflow, 60, None).await,
            Err(RecurringError::Invalid(JobError::EmptyKeyword))
        ));
        for i in 0..20 {
            service
                .create(user, &format!("k{i}"), JobMode::Workflow, 60, None)
                .await
                .unwrap();
        }
        assert!(matches!(
            service
                .create(user, "one too many", JobMode::Workflow, 60, None)
                .await,
            Err(RecurringError::TooMany(20))
        ));
    }
}
