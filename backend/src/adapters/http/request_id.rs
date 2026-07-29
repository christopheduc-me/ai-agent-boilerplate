//! Correlation id middleware (ADR-018).
//!
//! Every request runs inside a tracing span carrying a `request_id` — taken
//! from the incoming `X-Request-Id` header (set by a proxy or a retrying
//! client) or generated. The id is echoed back on the response so clients and
//! the reverse proxy can log the same value. For the asynchronous research
//! flow, the `job_id` is the cross-service correlation key: the dispatcher
//! forwards it as `X-Request-Id` to the agent (see `adapters/dispatch`).

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::trace::{TraceContextExt, TraceId};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

const HEADER: &str = "x-request-id";

/// Records the OpenTelemetry trace id assigned to `span` (ADR-029) as a field,
/// so JSON logs cross-reference the trace. A no-op when tracing is off: the
/// span then has no OTel context and the id stays `INVALID`, leaving the field
/// empty (so the keyless output is unchanged).
fn record_otel_trace_id(span: &tracing::Span) {
    let trace_id = span.context().span().span_context().trace_id();
    if trace_id != TraceId::INVALID {
        span.record("trace_id", tracing::field::display(trace_id));
    }
}

fn incoming_id(request: &Request) -> Option<String> {
    let value = request.headers().get(HEADER)?.to_str().ok()?.trim();
    // Only accept sane values; anything else gets replaced by a fresh id.
    (!value.is_empty() && value.len() <= 128 && value.chars().all(|c| c.is_ascii_graphic()))
        .then(|| value.to_string())
}

pub async fn request_id(request: Request, next: Next) -> Response {
    let id = incoming_id(&request).unwrap_or_else(|| Uuid::new_v4().to_string());
    let span = tracing::info_span!(
        "http_request",
        request_id = %id,
        method = %request.method(),
        path = %request.uri().path(),
        // Populated below when OpenTelemetry is on (ADR-029), so every log line
        // under this span carries the trace id and links back to Jaeger.
        trace_id = tracing::field::Empty,
    );
    record_otel_trace_id(&span);
    let mut response = next.run(request).instrument(span).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(HEADER, value);
    }
    response
}
