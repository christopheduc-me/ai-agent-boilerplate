"""Structured logging (ADR-018): one JSON object per line, extras included."""

import json
import logging

from opentelemetry.sdk.trace import TracerProvider

from aiagent.logging_setup import JsonFormatter, TraceContextFilter, configure_logging


def format_record(**extra: str) -> dict:
    record = logging.LogRecord(
        name="aiagent.test",
        level=logging.INFO,
        pathname=__file__,
        lineno=1,
        msg="something happened",
        args=None,
        exc_info=None,
    )
    for key, value in extra.items():
        setattr(record, key, value)
    return json.loads(JsonFormatter().format(record))


def test_json_formatter_emits_parseable_json_with_extras() -> None:
    payload = format_record(request_id="corr-42", job_id="j1")

    assert payload["message"] == "something happened"
    assert payload["level"] == "INFO"
    assert payload["logger"] == "aiagent.test"
    assert payload["request_id"] == "corr-42"
    assert payload["job_id"] == "j1"
    assert "timestamp" in payload


def a_record() -> logging.LogRecord:
    return logging.LogRecord("aiagent.test", logging.INFO, __file__, 1, "msg", None, None)


def test_trace_filter_is_a_noop_without_an_active_span() -> None:
    record = a_record()
    assert TraceContextFilter().filter(record) is True
    assert not hasattr(record, "trace_id")  # nothing added when tracing is off


def test_trace_filter_stamps_the_active_trace_and_span_ids() -> None:
    # A local provider (never set global — keeps other suites' providers intact)
    # still makes its span the current one via the context API.
    tracer = TracerProvider().get_tracer("test")
    record = a_record()
    with tracer.start_as_current_span("op"):
        TraceContextFilter().filter(record)
    assert len(record.trace_id) == 32  # 128-bit trace id, hex
    assert len(record.span_id) == 16  # 64-bit span id, hex
    # The stamped ids land in the JSON payload for log/trace cross-reference.
    payload = json.loads(JsonFormatter().format(record))
    assert payload["trace_id"] == record.trace_id


def test_configure_logging_switches_on_log_format(monkeypatch) -> None:
    monkeypatch.setenv("LOG_FORMAT", "json")
    configure_logging()
    assert isinstance(logging.getLogger().handlers[0].formatter, JsonFormatter)

    monkeypatch.delenv("LOG_FORMAT")
    configure_logging()
    assert not isinstance(logging.getLogger().handlers[0].formatter, JsonFormatter)
