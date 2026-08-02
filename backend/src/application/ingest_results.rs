use std::sync::Arc;

use uuid::Uuid;

use crate::domain::ports::{
    ChannelNotifier, Digest, DigestEntry, DigestSender, JobRepository,
    NotificationChannelRepository, PortError, RecurringSearchRepository,
};
use crate::domain::{AgentStep, JobUsage, ResearchJob, SearchResult};

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
    recurring: Arc<dyn RecurringSearchRepository>,
    digests: Arc<dyn DigestSender>,
    channels: Arc<dyn NotificationChannelRepository>,
    notifier: Arc<dyn ChannelNotifier>,
}

impl IngestResults {
    pub fn new(
        jobs: Arc<dyn JobRepository>,
        recurring: Arc<dyn RecurringSearchRepository>,
        digests: Arc<dyn DigestSender>,
        channels: Arc<dyn NotificationChannelRepository>,
        notifier: Arc<dyn ChannelNotifier>,
    ) -> Self {
        Self {
            jobs,
            recurring,
            digests,
            channels,
            notifier,
        }
    }

    /// Digest hook (ADR-036/061): a recurring run that delivered new results
    /// notifies the recurring search's saved webhook (if any) **and** every
    /// notification channel in the owner's profile (Slack, Telegram…). Strictly
    /// best-effort — a dead webhook/channel (or a deleted search) never fails
    /// the ingestion; each destination is tried independently.
    async fn maybe_send_digest(&self, job: &ResearchJob, results: &[SearchResult]) {
        let Some(rs_id) = job.recurring_search_id else {
            return;
        };
        let new_results: Vec<&SearchResult> = results.iter().filter(|r| r.is_new).collect();
        if new_results.is_empty() {
            return; // nothing new since the last run: no notification
        }
        let search = match self.recurring.find(rs_id).await {
            Ok(Some(search)) => search,
            Ok(None) => return, // deleted meanwhile
            Err(e) => {
                tracing::warn!(error = %e, "digest lookup failed");
                return;
            }
        };
        let digest = Digest {
            recurring_search_id: rs_id,
            job_id: job.id,
            keyword: job.keyword.clone(),
            new_count: new_results.len(),
            new_results: new_results
                .iter()
                .map(|r| DigestEntry {
                    title: r.title.clone(),
                    url: r.url.clone(),
                    published_at: r.published_at,
                })
                .collect(),
        };
        // The recurring search's own webhook (ADR-036), if set.
        if let Some(url) = &search.webhook_url {
            if let Err(e) = self.digests.send(url, &digest).await {
                tracing::warn!(job_id = %job.id, error = %e, "digest webhook delivery failed");
            }
        }
        // The owner's profile channels (ADR-061).
        match self.channels.list_for_user(search.user_id).await {
            Ok(channels) => {
                for channel in &channels {
                    if let Err(e) = self.notifier.notify(channel, &digest).await {
                        tracing::warn!(
                            job_id = %job.id,
                            kind = channel.kind.as_str(),
                            error = %e,
                            "digest channel delivery failed"
                        );
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "channel lookup failed"),
        }
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
        self.maybe_send_digest(&job, results).await;
        Ok(())
    }

    /// Records one decision of the agentic loop (ADR-030). Idempotent on
    /// `(job_id, seq)` — Celery retries re-send the same journal entries.
    pub async fn record_step(&self, job_id: Uuid, step: &AgentStep) -> Result<(), IngestError> {
        self.jobs
            .find(job_id)
            .await?
            .ok_or(IngestError::JobNotFound)?;
        self.jobs.append_step(job_id, step).await?;
        Ok(())
    }

    /// The agent asked a clarification question (ADR-032): the job pauses in
    /// `awaiting_input`. Idempotent like every worker notification.
    pub async fn request_input(&self, job_id: Uuid, question: &str) -> Result<(), IngestError> {
        let mut job = self
            .jobs
            .find(job_id)
            .await?
            .ok_or(IngestError::JobNotFound)?;
        job.request_input(question)
            .map_err(|e| IngestError::Infrastructure(PortError(e.to_string())))?;
        self.jobs.update(&job).await?;
        Ok(())
    }

    /// Accumulates one task attempt's spend (ADR-038): retries and HITL
    /// resumes each add their own real cost.
    pub async fn record_usage(&self, job_id: Uuid, usage: &JobUsage) -> Result<(), IngestError> {
        self.jobs
            .find(job_id)
            .await?
            .ok_or(IngestError::JobNotFound)?;
        self.jobs.add_usage(job_id, usage).await?;
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
    use crate::adapters::notify::NoopChannelNotifier;
    use crate::adapters::persistence::in_memory::{
        InMemoryJobRepository, InMemoryNotificationChannelRepository,
        InMemoryRecurringSearchRepository,
    };
    use crate::domain::{
        ChannelKind, DateConfidence, JobMode, JobStatus, NotificationChannel, RecurringSearch,
        ResearchJob,
    };
    use std::sync::Mutex;

    /// Records digest deliveries for assertions.
    #[derive(Default)]
    struct RecordingDigestSender {
        sent: Mutex<Vec<(String, Digest)>>,
    }

    #[async_trait::async_trait]
    impl DigestSender for RecordingDigestSender {
        async fn send(&self, url: &str, digest: &Digest) -> Result<(), PortError> {
            self.sent.lock().unwrap().push((url.into(), digest.clone()));
            Ok(())
        }
    }

    /// Records channel deliveries (kind, target) for assertions (ADR-061).
    #[derive(Default)]
    struct RecordingChannelNotifier {
        sent: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl ChannelNotifier for RecordingChannelNotifier {
        async fn notify(
            &self,
            channel: &NotificationChannel,
            _digest: &Digest,
        ) -> Result<(), PortError> {
            self.sent
                .lock()
                .unwrap()
                .push((channel.kind.as_str().into(), channel.target.clone()));
            Ok(())
        }
    }

    fn ingest_with(
        jobs: Arc<InMemoryJobRepository>,
        recurring: Arc<InMemoryRecurringSearchRepository>,
        digests: Arc<RecordingDigestSender>,
    ) -> IngestResults {
        IngestResults::new(
            jobs,
            recurring,
            digests,
            Arc::new(InMemoryNotificationChannelRepository::default()),
            Arc::new(NoopChannelNotifier),
        )
    }

    /// Same, but with explicit channels + notifier to assert profile delivery.
    fn ingest_with_channels(
        jobs: Arc<InMemoryJobRepository>,
        recurring: Arc<InMemoryRecurringSearchRepository>,
        channels: Arc<InMemoryNotificationChannelRepository>,
        notifier: Arc<RecordingChannelNotifier>,
    ) -> IngestResults {
        IngestResults::new(
            jobs,
            recurring,
            Arc::new(RecordingDigestSender::default()),
            channels,
            notifier,
        )
    }

    fn a_result(title: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: "https://example.com".into(),
            snippet: "...".into(),
            published_at: None,
            date_confidence: DateConfidence::Unknown,
            event_type: crate::domain::EventType::default(),
            summary: None,
            is_new: true,
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
        let ingest = ingest_with(
            jobs.clone(),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );

        ingest.start(job.id).await.unwrap();

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Running);
    }

    #[tokio::test]
    async fn late_start_does_not_reopen_a_finished_job() {
        let (jobs, job) = repo_with_job().await;
        let ingest = ingest_with(
            jobs.clone(),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );

        ingest.complete(job.id, &[a_result("r")]).await.unwrap();
        ingest.start(job.id).await.unwrap(); // Celery retry after completion

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Completed);
    }

    #[tokio::test]
    async fn stores_results_and_completes_the_job() {
        let (jobs, job) = repo_with_job().await;
        let ingest = ingest_with(
            jobs.clone(),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );

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
        let ingest = ingest_with(
            jobs.clone(),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );

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
        let ingest = ingest_with(
            jobs,
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );

        let err = ingest.complete(Uuid::new_v4(), &[]).await.unwrap_err();
        assert!(matches!(err, IngestError::JobNotFound));
    }

    fn a_step(seq: i32, kind: &str) -> AgentStep {
        AgentStep {
            seq,
            kind: kind.into(),
            detail: "rust".into(),
            reason: "because".into(),
            new_hits: 2,
        }
    }

    #[tokio::test]
    async fn records_agent_steps_idempotently() {
        let (jobs, job) = repo_with_job().await;
        let ingest = ingest_with(
            jobs.clone(),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );

        ingest
            .record_step(job.id, &a_step(1, "search"))
            .await
            .unwrap();
        ingest
            .record_step(job.id, &a_step(1, "search"))
            .await
            .unwrap(); // retry
        ingest
            .record_step(job.id, &a_step(2, "finish"))
            .await
            .unwrap();

        let steps = jobs.steps_for(job.id).await.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps
                .iter()
                .map(|s| (s.seq, s.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "search"), (2, "finish")]
        );
    }

    #[tokio::test]
    async fn request_input_pauses_a_running_job() {
        let (jobs, job) = repo_with_job().await;
        let ingest = ingest_with(
            jobs.clone(),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );
        ingest.start(job.id).await.unwrap();

        ingest
            .request_input(job.id, "The animal or the car?")
            .await
            .unwrap();

        let stored = jobs.find(job.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::AwaitingInput);
        assert_eq!(stored.question.as_deref(), Some("The animal or the car?"));
    }

    #[tokio::test]
    async fn step_for_an_unknown_job_is_an_error() {
        let ingest = ingest_with(
            Arc::new(InMemoryJobRepository::default()),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(RecordingDigestSender::default()),
        );
        let err = ingest
            .record_step(Uuid::new_v4(), &a_step(1, "search"))
            .await
            .unwrap_err();
        assert!(matches!(err, IngestError::JobNotFound));
    }

    #[tokio::test]
    async fn recurring_run_with_news_delivers_a_digest() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let recurring = Arc::new(InMemoryRecurringSearchRepository::default());
        let digests = Arc::new(RecordingDigestSender::default());
        let ingest = ingest_with(jobs.clone(), recurring.clone(), digests.clone());

        let rs = RecurringSearch::new(
            Uuid::new_v4(),
            "rust releases",
            JobMode::Agent,
            60,
            Some("https://hooks.example.com/digest"),
        )
        .unwrap();
        recurring.insert(&rs).await.unwrap();
        let job = ResearchJob::new(rs.user_id, "rust releases")
            .unwrap()
            .with_recurring(rs.id);
        jobs.insert(&job).await.unwrap();

        let mut seen = a_result("already-seen");
        seen.is_new = false;
        ingest
            .complete(job.id, &[a_result("fresh"), seen])
            .await
            .unwrap();

        let sent = digests.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let (url, digest) = &sent[0];
        assert_eq!(url, "https://hooks.example.com/digest");
        assert_eq!(digest.keyword, "rust releases");
        assert_eq!(digest.new_count, 1);
        // Only the NEW results ride in the digest.
        assert_eq!(digest.new_results[0].title, "fresh");
    }

    #[tokio::test]
    async fn recurring_run_notifies_the_owner_profile_channels() {
        // ADR-061: a recurring run with news notifies the owner's channels
        // (Slack, Telegram…) in addition to the search webhook.
        let jobs = Arc::new(InMemoryJobRepository::default());
        let recurring = Arc::new(InMemoryRecurringSearchRepository::default());
        let channels = Arc::new(InMemoryNotificationChannelRepository::default());
        let notifier = Arc::new(RecordingChannelNotifier::default());
        let ingest = ingest_with_channels(
            jobs.clone(),
            recurring.clone(),
            channels.clone(),
            notifier.clone(),
        );

        let user_id = Uuid::new_v4();
        let rs = RecurringSearch::new(user_id, "rust", JobMode::Agent, 60, None).unwrap();
        recurring.insert(&rs).await.unwrap();
        // Two channels in the owner's profile.
        channels
            .insert(
                &NotificationChannel::new(user_id, ChannelKind::Slack, "https://hooks/x", None)
                    .unwrap(),
            )
            .await
            .unwrap();
        channels
            .insert(
                &NotificationChannel::new(user_id, ChannelKind::Telegram, "chat1", Some("tok"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // A different user's channel must NOT be notified.
        channels
            .insert(
                &NotificationChannel::new(
                    Uuid::new_v4(),
                    ChannelKind::Slack,
                    "https://hooks/other",
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let job = ResearchJob::new(user_id, "rust")
            .unwrap()
            .with_recurring(rs.id);
        jobs.insert(&job).await.unwrap();
        ingest.complete(job.id, &[a_result("fresh")]).await.unwrap();

        let sent = notifier.sent.lock().unwrap();
        assert_eq!(sent.len(), 2, "only the owner's two channels");
        assert!(sent.contains(&("slack".into(), "https://hooks/x".into())));
        assert!(sent.contains(&("telegram".into(), "chat1".into())));
    }

    #[tokio::test]
    async fn no_digest_without_news_webhook_or_recurring_link() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let recurring = Arc::new(InMemoryRecurringSearchRepository::default());
        let digests = Arc::new(RecordingDigestSender::default());
        let ingest = ingest_with(jobs.clone(), recurring.clone(), digests.clone());

        // One-shot job: never a digest, even with new results.
        let one_shot = ResearchJob::new(Uuid::new_v4(), "k").unwrap();
        jobs.insert(&one_shot).await.unwrap();
        ingest
            .complete(one_shot.id, &[a_result("r")])
            .await
            .unwrap();

        // Recurring without a webhook: no digest.
        let silent = RecurringSearch::new(Uuid::new_v4(), "k", JobMode::Agent, 60, None).unwrap();
        recurring.insert(&silent).await.unwrap();
        let job = ResearchJob::new(silent.user_id, "k")
            .unwrap()
            .with_recurring(silent.id);
        jobs.insert(&job).await.unwrap();
        ingest.complete(job.id, &[a_result("r")]).await.unwrap();

        // Recurring with a webhook but nothing new: no digest.
        let hooked = RecurringSearch::new(
            Uuid::new_v4(),
            "k",
            JobMode::Agent,
            60,
            Some("https://hooks.example.com/x"),
        )
        .unwrap();
        recurring.insert(&hooked).await.unwrap();
        let job2 = ResearchJob::new(hooked.user_id, "k")
            .unwrap()
            .with_recurring(hooked.id);
        jobs.insert(&job2).await.unwrap();
        let mut old = a_result("old");
        old.is_new = false;
        ingest.complete(job2.id, &[old]).await.unwrap();

        assert!(digests.sent.lock().unwrap().is_empty());
    }
}
