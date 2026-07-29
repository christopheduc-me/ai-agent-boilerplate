"""Structured logging (ADR-018), stdlib only.

`LOG_FORMAT=json` switches every process (FastAPI, Celery worker) to one JSON
object per line, carrying any `extra={...}` fields — notably `request_id` and
`job_id`, the correlation keys propagated from the Rust backend. Anything else
keeps the human-readable default.

When OpenTelemetry is enabled (ADR-029), each line also carries the active
`trace_id`/`span_id`, so a log line links to its Jaeger trace and back.
"""

import json
import logging
import os
from datetime import UTC, datetime
from typing import Any

from opentelemetry import trace

# logging.LogRecord attributes that are not user-provided extras.
_RESERVED = frozenset(logging.LogRecord("", 0, "", 0, "", None, None).__dict__) | {
    "message",
    "asctime",
    "taskName",
}


class TraceContextFilter(logging.Filter):
    """Stamps each record with the active OpenTelemetry trace/span id (ADR-029
    amendment) so logs and traces cross-reference. A no-op when tracing is off
    (no active span) — the fields are simply absent, keeping the keyless output
    unchanged."""

    def filter(self, record: logging.LogRecord) -> bool:
        context = trace.get_current_span().get_span_context()
        if context.is_valid:
            record.trace_id = trace.format_trace_id(context.trace_id)
            record.span_id = trace.format_span_id(context.span_id)
        return True


class JsonFormatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:
        payload: dict[str, Any] = {
            "timestamp": datetime.fromtimestamp(record.created, tz=UTC).isoformat(),
            "level": record.levelname,
            "logger": record.name,
            "message": record.getMessage(),
        }
        payload.update({k: v for k, v in record.__dict__.items() if k not in _RESERVED})
        if record.exc_info and record.exc_info[0] is not None:
            payload["exception"] = self.formatException(record.exc_info)
        return json.dumps(payload, default=str)


def configure_logging() -> None:
    """Idempotent root-logger setup driven by LOG_FORMAT."""
    handler = logging.StreamHandler()
    if os.environ.get("LOG_FORMAT") == "json":
        handler.setFormatter(JsonFormatter())
    else:
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s %(message)s"))
    handler.addFilter(TraceContextFilter())
    root = logging.getLogger()
    root.handlers = [handler]
    root.setLevel(os.environ.get("LOG_LEVEL", "INFO").upper())
