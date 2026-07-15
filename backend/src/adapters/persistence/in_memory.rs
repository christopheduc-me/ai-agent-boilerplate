use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::ports::{JobRepository, PortError, RefreshTokenRepository, UserRepository};
use crate::domain::{AgentStep, JobStatus, RefreshToken, ResearchJob, SearchResult, User};

#[derive(Default)]
pub struct InMemoryUserRepository {
    users: Mutex<HashMap<Uuid, User>>,
}

#[async_trait]
impl UserRepository for InMemoryUserRepository {
    async fn insert(&self, user: &User) -> Result<(), PortError> {
        self.users.lock().unwrap().insert(user.id, user.clone());
        Ok(())
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<User>, PortError> {
        Ok(self
            .users
            .lock()
            .unwrap()
            .values()
            .find(|u| u.email == email)
            .cloned())
    }
}

#[derive(Default)]
pub struct InMemoryRefreshTokenRepository {
    tokens: Mutex<HashMap<Uuid, RefreshToken>>,
}

#[async_trait]
impl RefreshTokenRepository for InMemoryRefreshTokenRepository {
    async fn insert(&self, token: &RefreshToken) -> Result<(), PortError> {
        self.tokens.lock().unwrap().insert(token.id, token.clone());
        Ok(())
    }

    async fn find_by_hash(&self, hash: &str) -> Result<Option<RefreshToken>, PortError> {
        Ok(self
            .tokens
            .lock()
            .unwrap()
            .values()
            .find(|t| t.token_hash == hash)
            .cloned())
    }

    async fn delete(&self, id: Uuid) -> Result<(), PortError> {
        self.tokens.lock().unwrap().remove(&id);
        Ok(())
    }

    async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, PortError> {
        let mut tokens = self.tokens.lock().unwrap();
        let before = tokens.len();
        tokens.retain(|_, t| !t.is_expired(now));
        Ok((before - tokens.len()) as u64)
    }
}

#[derive(Default)]
pub struct InMemoryJobRepository {
    jobs: Mutex<HashMap<Uuid, ResearchJob>>,
    results: Mutex<HashMap<Uuid, Vec<SearchResult>>>,
    steps: Mutex<HashMap<Uuid, Vec<AgentStep>>>,
}

#[async_trait]
impl JobRepository for InMemoryJobRepository {
    async fn insert(&self, job: &ResearchJob) -> Result<(), PortError> {
        self.jobs.lock().unwrap().insert(job.id, job.clone());
        Ok(())
    }

    async fn update(&self, job: &ResearchJob) -> Result<(), PortError> {
        self.jobs.lock().unwrap().insert(job.id, job.clone());
        Ok(())
    }

    async fn find(&self, id: Uuid) -> Result<Option<ResearchJob>, PortError> {
        Ok(self.jobs.lock().unwrap().get(&id).cloned())
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ResearchJob>, PortError> {
        let mut jobs: Vec<ResearchJob> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| j.user_id == user_id)
            .cloned()
            .collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
        Ok(jobs)
    }

    async fn count_created_since(
        &self,
        user_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<u64, PortError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| j.user_id == user_id && j.created_at >= since)
            .count() as u64)
    }

    async fn list_unfinished_older_than(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<Vec<ResearchJob>, PortError> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .values()
            // awaiting_input is paused on the user, not stuck (ADR-032):
            // the reaper only targets pending/running.
            .filter(|j| {
                matches!(j.status, JobStatus::Pending | JobStatus::Running) && j.created_at < cutoff
            })
            .cloned()
            .collect())
    }

    async fn store_results(&self, job_id: Uuid, results: &[SearchResult]) -> Result<(), PortError> {
        self.results
            .lock()
            .unwrap()
            .insert(job_id, results.to_vec());
        Ok(())
    }

    async fn results_for(&self, job_id: Uuid) -> Result<Vec<SearchResult>, PortError> {
        Ok(self
            .results
            .lock()
            .unwrap()
            .get(&job_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn append_step(&self, job_id: Uuid, step: &AgentStep) -> Result<(), PortError> {
        let mut steps = self.steps.lock().unwrap();
        let journal = steps.entry(job_id).or_default();
        // Idempotent on (job_id, seq): a Celery retry re-sends the same step.
        if !journal.iter().any(|s| s.seq == step.seq) {
            journal.push(step.clone());
            journal.sort_by_key(|s| s.seq);
        }
        Ok(())
    }

    async fn steps_for(&self, job_id: Uuid) -> Result<Vec<AgentStep>, PortError> {
        Ok(self
            .steps
            .lock()
            .unwrap()
            .get(&job_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn clear_steps(&self, job_id: Uuid) -> Result<(), PortError> {
        self.steps.lock().unwrap().remove(&job_id);
        Ok(())
    }
}
