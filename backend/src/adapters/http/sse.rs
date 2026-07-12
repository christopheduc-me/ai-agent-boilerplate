//! Server-Sent Events stream for job updates (ADR-026).
//!
//! Each connection polls the repository (through the ownership-enforcing
//! `SearchQueries` use case) and emits an `update` event whenever the job
//! detail changes, closing after the terminal state. Polling the database —
//! rather than an in-process broadcast — keeps the stream correct when the
//! backend scales to several instances (state lives in PostgreSQL).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::Event;
use futures_util::stream::{unfold, Stream};
use uuid::Uuid;

use crate::application::SearchQueries;

/// How often each open stream re-reads the job (per connection).
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

struct StreamState {
    queries: Arc<SearchQueries>,
    user_id: Uuid,
    job_id: Uuid,
    last_payload: Option<String>,
    finished: bool,
}

/// Emits the current job detail immediately, then one `update` event per
/// change; ends after the event carrying a terminal status (or if the job
/// becomes unreadable).
pub fn job_updates(
    queries: Arc<SearchQueries>,
    user_id: Uuid,
    job_id: Uuid,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let state = StreamState {
        queries,
        user_id,
        job_id,
        last_payload: None,
        finished: false,
    };
    unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }
        loop {
            let Ok(Some((job, results, steps))) =
                state.queries.get(state.user_id, state.job_id).await
            else {
                return None; // job vanished or infrastructure error: end the stream
            };
            let payload = super::job_detail_json(&job, &results, &steps).to_string();
            if state.last_payload.as_deref() != Some(&payload) {
                state.last_payload = Some(payload.clone());
                state.finished = job.is_finished();
                let event = Event::default().event("update").data(payload);
                return Some((Ok(event), state));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
}
