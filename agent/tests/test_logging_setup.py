"""Structured logging (ADR-018): one JSON object per line, extras included."""

import json
import logging

from aiagent.logging_setup import JsonFormatter, configure_logging


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


def test_configure_logging_switches_on_log_format(monkeypatch) -> None:
    monkeypatch.setenv("LOG_FORMAT", "json")
    configure_logging()
    assert isinstance(logging.getLogger().handlers[0].formatter, JsonFormatter)

    monkeypatch.delenv("LOG_FORMAT")
    configure_logging()
    assert not isinstance(logging.getLogger().handlers[0].formatter, JsonFormatter)
