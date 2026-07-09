//! Outbound adapters implementing `JobDispatcher` (ADR-005).

use async_trait::async_trait;
use serde::Serialize;

use crate::domain::ports::{JobDispatcher, PortError};
use crate::domain::ResearchJob;

/// Dispatches jobs to the FastAPI micro-API, which enqueues them via Celery.
pub struct HttpJobDispatcher {
    client: reqwest::Client,
    base_url: String,
    internal_token: String,
}

#[derive(Serialize)]
struct TaskRequest<'a> {
    job_id: uuid::Uuid,
    keyword: &'a str,
}

impl HttpJobDispatcher {
    pub fn new(base_url: String, internal_token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            internal_token,
        }
    }
}

#[async_trait]
impl JobDispatcher for HttpJobDispatcher {
    async fn dispatch(&self, job: &ResearchJob) -> Result<(), PortError> {
        let response = self
            .client
            .post(format!("{}/tasks", self.base_url))
            .header("X-Internal-Token", &self.internal_token)
            // Correlation (ADR-018): the job id follows the work through
            // FastAPI, Celery, and the worker's callbacks.
            .header("X-Request-Id", job.id.to_string())
            .json(&TaskRequest {
                job_id: job.id,
                keyword: &job.keyword,
            })
            .send()
            .await
            .map_err(|e| PortError(format!("agent API unreachable: {e}")))?;

        if !response.status().is_success() {
            return Err(PortError(format!(
                "agent API returned {}",
                response.status()
            )));
        }
        Ok(())
    }
}

/// No-op dispatcher for local development without the agent stack
/// (`AGENT_API_URL` unset) and for tests. Jobs stay `pending` forever.
#[derive(Default)]
pub struct NoopJobDispatcher;

#[async_trait]
impl JobDispatcher for NoopJobDispatcher {
    async fn dispatch(&self, job: &ResearchJob) -> Result<(), PortError> {
        tracing::warn!(job_id = %job.id, "NoopJobDispatcher: job not sent to any agent");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    type SeenHeaders = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;

    /// Spawns a stub agent API capturing the auth + correlation headers.
    async fn spawn_stub() -> (String, SeenHeaders) {
        let seen: SeenHeaders = Arc::new(Mutex::new(vec![]));
        let app = Router::new()
            .route(
                "/tasks",
                post(
                    |State(seen): State<SeenHeaders>, headers: HeaderMap| async move {
                        let get = |name: &str| {
                            headers
                                .get(name)
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string)
                        };
                        seen.lock()
                            .unwrap()
                            .push((get("x-internal-token"), get("x-request-id")));
                        "queued"
                    },
                ),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn dispatch_sends_internal_token_and_correlation_id() {
        let (base_url, seen) = spawn_stub().await;
        let dispatcher = HttpJobDispatcher::new(base_url, "secret".into());
        let job = ResearchJob::new(Uuid::new_v4(), "keyword").unwrap();

        dispatcher.dispatch(&job).await.unwrap();

        let calls = seen.lock().unwrap();
        assert_eq!(
            calls.as_slice(),
            &[(Some("secret".into()), Some(job.id.to_string()))]
        );
    }
}
