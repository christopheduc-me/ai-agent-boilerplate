//! Profile use case (ADR-061): the logged-in user views their account and
//! manages where they receive digests (notification channels).

use std::sync::Arc;

use uuid::Uuid;

use crate::domain::notification_channel::ChannelError;
use crate::domain::ports::{NotificationChannelRepository, PortError, UserRepository};
use crate::domain::{ChannelKind, NotificationChannel, User};

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("account not found")]
    NotFound,
    #[error("email delivery is not configured on this server")]
    EmailNotConfigured,
    #[error(transparent)]
    InvalidChannel(#[from] ChannelError),
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

pub struct Profile {
    users: Arc<dyn UserRepository>,
    channels: Arc<dyn NotificationChannelRepository>,
    /// Whether SMTP is configured (ADR-062): gates creating an `email` channel,
    /// and surfaced in the profile so the UI can hide the option.
    email_enabled: bool,
}

impl Profile {
    pub fn new(
        users: Arc<dyn UserRepository>,
        channels: Arc<dyn NotificationChannelRepository>,
        email_enabled: bool,
    ) -> Self {
        Self {
            users,
            channels,
            email_enabled,
        }
    }

    pub fn email_enabled(&self) -> bool {
        self.email_enabled
    }

    /// The account plus its notification channels.
    pub async fn view(
        &self,
        user_id: Uuid,
    ) -> Result<(User, Vec<NotificationChannel>), ProfileError> {
        let user = self
            .users
            .find_by_id(user_id)
            .await?
            .ok_or(ProfileError::NotFound)?;
        let channels = self.channels.list_for_user(user_id).await?;
        Ok((user, channels))
    }

    pub async fn add_channel(
        &self,
        user_id: Uuid,
        kind: ChannelKind,
        target: &str,
        secret: Option<&str>,
    ) -> Result<NotificationChannel, ProfileError> {
        if kind == ChannelKind::Email && !self.email_enabled {
            return Err(ProfileError::EmailNotConfigured);
        }
        let channel = NotificationChannel::new(user_id, kind, target, secret)?;
        self.channels.insert(&channel).await?;
        Ok(channel)
    }

    /// Deletes the user's channel; false when unknown or foreign.
    pub async fn remove_channel(&self, user_id: Uuid, id: Uuid) -> Result<bool, PortError> {
        self.channels.delete(user_id, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::{
        InMemoryNotificationChannelRepository, InMemoryUserRepository,
    };

    async fn profile_with_user() -> (Profile, Uuid) {
        profile_with_email(true).await
    }

    async fn profile_with_email(email_enabled: bool) -> (Profile, Uuid) {
        let users = Arc::new(InMemoryUserRepository::default());
        let channels = Arc::new(InMemoryNotificationChannelRepository::default());
        let user = User::new("me@b.com".into(), "hash".into());
        users.insert(&user).await.unwrap();
        (Profile::new(users, channels, email_enabled), user.id)
    }

    #[tokio::test]
    async fn add_list_and_remove_channels() {
        let (profile, user_id) = profile_with_user().await;

        let ch = profile
            .add_channel(user_id, ChannelKind::Telegram, "chat1", Some("tok"))
            .await
            .unwrap();

        let (user, channels) = profile.view(user_id).await.unwrap();
        assert_eq!(user.email, "me@b.com");
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].kind, ChannelKind::Telegram);

        assert!(profile.remove_channel(user_id, ch.id).await.unwrap());
        assert!(profile.view(user_id).await.unwrap().1.is_empty());
    }

    #[tokio::test]
    async fn invalid_channel_is_rejected() {
        let (profile, user_id) = profile_with_user().await;
        let err = profile
            .add_channel(user_id, ChannelKind::Slack, "http://insecure", None)
            .await
            .unwrap_err();
        assert!(matches!(err, ProfileError::InvalidChannel(_)));
    }

    #[tokio::test]
    async fn email_channel_is_gated_on_smtp_being_configured() {
        // ADR-062: with SMTP off, adding an email channel is refused clearly.
        let (off, user_id) = profile_with_email(false).await;
        assert!(!off.email_enabled());
        assert!(matches!(
            off.add_channel(user_id, ChannelKind::Email, "me@b.com", None)
                .await
                .unwrap_err(),
            ProfileError::EmailNotConfigured
        ));

        // With SMTP on it goes through.
        let (on, user_id) = profile_with_email(true).await;
        assert!(on.email_enabled());
        on.add_channel(user_id, ChannelKind::Email, "me@b.com", None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn view_of_a_missing_account_is_not_found() {
        let (profile, _) = profile_with_user().await;
        assert!(matches!(
            profile.view(Uuid::new_v4()).await.unwrap_err(),
            ProfileError::NotFound
        ));
    }
}
