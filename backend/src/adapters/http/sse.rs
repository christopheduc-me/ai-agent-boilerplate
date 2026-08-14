//! Server-Sent Events stream for job updates (ADR-026).
//!
//! Each connection polls the repository (through the ownership-enforcing
//! `SearchQueries` use case) and emits an `update` event whenever the job
//! detail changes, closing after the terminal state. Polling the database —
//! rather than an in-process broadcast — keeps the stream correct when the
//! backend scales to several instances (state lives in PostgreSQL).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::response::sse::Event;
use futures_util::stream::{unfold, Stream};
use uuid::Uuid;

use crate::application::SearchQueries;
use crate::domain::JobStatus;

/// How often each open stream re-reads a job that is still progressing.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Slower cadence while the job waits on a human (ADR-070). An `awaiting_input`
/// job (ADR-032) changes only when the user posts the answer — and that client
/// gets its own HTTP response — so polling it every second buys nothing. It also
/// never ends on its own: `is_finished()` covers only `Completed`/`Failed`, and
/// the reaper deliberately skips this status, so at 1 Hz an open tab would read
/// the database forever.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Upper bound on one stream (ADR-070). The client already falls back to
/// polling (ADR-026), so closing is safe; without it a stream on a job that
/// never reaches a terminal state lives as long as the browser tab.
const MAX_STREAM_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// How long to wait before re-reading a job that has not changed. Split out so
/// the decision is unit-testable without waiting on a clock.
fn poll_interval(status: JobStatus, idle: Duration, active: Duration) -> Duration {
    match status {
        JobStatus::AwaitingInput => idle,
        _ => active,
    }
}

struct StreamState {
    queries: Arc<SearchQueries>,
    user_id: Uuid,
    job_id: Uuid,
    last_payload: Option<String>,
    finished: bool,
    started: Instant,
    max_lifetime: Duration,
    idle_interval: Duration,
    active_interval: Duration,
}

/// Emits the current job detail immediately, then one `update` event per
/// change; ends after the event carrying a terminal status (or if the job
/// becomes unreadable).
pub fn job_updates(
    queries: Arc<SearchQueries>,
    user_id: Uuid,
    job_id: Uuid,
) -> impl Stream<Item = Result<Event, Infallible>> {
    job_updates_with(
        queries,
        user_id,
        job_id,
        MAX_STREAM_LIFETIME,
        IDLE_POLL_INTERVAL,
        POLL_INTERVAL,
    )
}

/// The durations are parameters so tests can drive the stream in milliseconds
/// instead of waiting out the real ones.
fn job_updates_with(
    queries: Arc<SearchQueries>,
    user_id: Uuid,
    job_id: Uuid,
    max_lifetime: Duration,
    idle_interval: Duration,
    active_interval: Duration,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let state = StreamState {
        queries,
        user_id,
        job_id,
        last_payload: None,
        finished: false,
        started: Instant::now(),
        max_lifetime,
        idle_interval,
        active_interval,
    };
    unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }
        loop {
            // Checked before each read, so a stream that will never see a
            // terminal status still ends (ADR-070).
            if state.started.elapsed() >= state.max_lifetime {
                return None;
            }
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
            let wait = poll_interval(job.status, state.idle_interval, state.active_interval);
            tokio::time::sleep(wait).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemoryJobRepository;
    use crate::domain::ports::JobRepository;
    use crate::domain::ResearchJob;
    use futures_util::StreamExt;

    fn intervals() -> (Duration, Duration) {
        (IDLE_POLL_INTERVAL, POLL_INTERVAL)
    }

    #[test]
    fn a_job_waiting_on_a_human_is_polled_far_less_often() {
        let (idle, active) = intervals();
        // The whole point of ADR-070: this status cannot change without the
        // user acting, and the reaper never ends it, so 1 Hz would run forever.
        assert_eq!(poll_interval(JobStatus::AwaitingInput, idle, active), idle);
        assert!(idle > active, "the idle cadence must actually be slower");
    }

    #[test]
    fn a_progressing_job_keeps_the_responsive_cadence() {
        let (idle, active) = intervals();
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
        ] {
            assert_eq!(
                poll_interval(status, idle, active),
                active,
                "{status:?} must not be slowed down"
            );
        }
    }

    #[tokio::test]
    async fn a_stream_on_a_job_that_never_finishes_still_ends() {
        let jobs = Arc::new(InMemoryJobRepository::default());
        let owner = Uuid::new_v4();
        let mut job = ResearchJob::new(owner, "ambiguous goal").unwrap();
        job.start();
        job.request_input("which one did you mean?").unwrap();
        assert!(!job.is_finished(), "the test premise: never terminal");
        jobs.insert(&job).await.unwrap();

        let queries = Arc::new(SearchQueries::new(jobs));
        let stream = job_updates_with(
            queries,
            owner,
            job.id,
            Duration::from_millis(150), // lifetime
            Duration::from_millis(10),  // idle
            Duration::from_millis(10),  // active
        );
        tokio::pin!(stream);

        // The first read always emits the current state; after that nothing
        // changes, so only the lifetime bound can end the stream.
        assert!(stream.next().await.is_some(), "expected the initial update");
        let ended = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        assert!(
            ended.expect("stream must end, not hang").is_none(),
            "the lifetime bound must close a stream that can never finish"
        );
    }
}
