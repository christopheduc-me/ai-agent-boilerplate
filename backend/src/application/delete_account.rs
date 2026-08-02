//! Account deletion (ADR-058): erases a user and all their data.
//!
//! The cascade is expressed here through the ports, not left to the database's
//! `ON DELETE CASCADE`, so the behavior is identical in-memory and on Postgres
//! and is unit-testable with fakes. The FK cascade stays as a safety net. Order
//! is children-first, user last, and every step is idempotent — a retry (or a
//! partial previous run) converges to "nothing left".

use std::sync::Arc;

use uuid::Uuid;

use crate::domain::ports::{
    JobRepository, NotificationChannelRepository, PortError, RecurringSearchRepository,
    RefreshTokenRepository, UserRepository,
};

pub struct DeleteAccount {
    users: Arc<dyn UserRepository>,
    jobs: Arc<dyn JobRepository>,
    recurring: Arc<dyn RecurringSearchRepository>,
    refresh_tokens: Arc<dyn RefreshTokenRepository>,
    channels: Arc<dyn NotificationChannelRepository>,
}

impl DeleteAccount {
    pub fn new(
        users: Arc<dyn UserRepository>,
        jobs: Arc<dyn JobRepository>,
        recurring: Arc<dyn RecurringSearchRepository>,
        refresh_tokens: Arc<dyn RefreshTokenRepository>,
        channels: Arc<dyn NotificationChannelRepository>,
    ) -> Self {
        Self {
            users,
            jobs,
            recurring,
            refresh_tokens,
            channels,
        }
    }

    pub async fn execute(&self, user_id: Uuid) -> Result<(), PortError> {
        self.recurring.delete_all_for_user(user_id).await?;
        self.jobs.delete_all_for_user(user_id).await?;
        self.refresh_tokens.delete_all_for_user(user_id).await?;
        self.channels.delete_all_for_user(user_id).await?;
        self.users.delete(user_id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::{
        InMemoryJobRepository, InMemoryNotificationChannelRepository,
        InMemoryRecurringSearchRepository, InMemoryRefreshTokenRepository, InMemoryUserRepository,
    };
    use crate::domain::ports::NotificationChannelRepository;
    use crate::domain::{
        ChannelKind, JobMode, NotificationChannel, RecurringSearch, RefreshToken, ResearchJob, User,
    };

    #[tokio::test]
    async fn deletes_the_user_and_all_their_data() {
        let users = Arc::new(InMemoryUserRepository::default());
        let jobs = Arc::new(InMemoryJobRepository::default());
        let recurring = Arc::new(InMemoryRecurringSearchRepository::default());
        let refresh = Arc::new(InMemoryRefreshTokenRepository::default());
        let channels = Arc::new(InMemoryNotificationChannelRepository::default());

        let user = User::new("gone@b.com".into(), "hash".into());
        users.insert(&user).await.unwrap();
        let job = ResearchJob::new(user.id, "k").unwrap();
        jobs.insert(&job).await.unwrap();
        let rs = RecurringSearch::new(user.id, "k", JobMode::Workflow, 60, None).unwrap();
        recurring.insert(&rs).await.unwrap();
        let (token, _) = RefreshToken::issue(user.id, 30);
        refresh.insert(&token).await.unwrap();
        let channel =
            NotificationChannel::new(user.id, ChannelKind::Slack, "https://hooks/x", None).unwrap();
        channels.insert(&channel).await.unwrap();

        // A second user's data must survive.
        let other = User::new("stay@b.com".into(), "hash".into());
        users.insert(&other).await.unwrap();
        let other_job = ResearchJob::new(other.id, "k2").unwrap();
        jobs.insert(&other_job).await.unwrap();

        DeleteAccount::new(
            users.clone(),
            jobs.clone(),
            recurring.clone(),
            refresh.clone(),
            channels.clone(),
        )
        .execute(user.id)
        .await
        .unwrap();

        assert!(users.find_by_email("gone@b.com").await.unwrap().is_none());
        assert!(jobs.find(job.id).await.unwrap().is_none());
        assert!(recurring.list_for_user(user.id).await.unwrap().is_empty());
        assert!(channels.list_for_user(user.id).await.unwrap().is_empty());
        assert!(refresh
            .find_by_hash(&token.token_hash)
            .await
            .unwrap()
            .is_none());

        // The other account is untouched.
        assert!(users.find_by_email("stay@b.com").await.unwrap().is_some());
        assert!(jobs.find(other_job.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn is_idempotent_for_an_unknown_user() {
        let users = Arc::new(InMemoryUserRepository::default());
        let jobs = Arc::new(InMemoryJobRepository::default());
        let recurring = Arc::new(InMemoryRecurringSearchRepository::default());
        let refresh = Arc::new(InMemoryRefreshTokenRepository::default());
        let channels = Arc::new(InMemoryNotificationChannelRepository::default());

        DeleteAccount::new(users, jobs, recurring, refresh, channels)
            .execute(Uuid::new_v4())
            .await
            .unwrap();
    }
}
