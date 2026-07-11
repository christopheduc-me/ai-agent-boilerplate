"""Telemetry gate (ADR-029): strictly opt-in, no network in tests."""

from opentelemetry import trace
from opentelemetry.instrumentation.celery import CeleryInstrumentor
from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor
from opentelemetry.sdk.trace import TracerProvider
from pytest import MonkeyPatch

from aiagent.telemetry import configure_telemetry


def test_disabled_without_the_endpoint_variable(monkeypatch: MonkeyPatch) -> None:
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    assert configure_telemetry("agent-test") is False


def test_enabled_when_the_endpoint_is_set(monkeypatch: MonkeyPatch) -> None:
    # The exporter connects lazily and no span is emitted here, so pointing at
    # an unused port never touches the network (ADR-012 test hygiene).
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:14318")
    try:
        assert configure_telemetry("agent-test") is True
        assert isinstance(trace.get_tracer_provider(), TracerProvider)
    finally:
        CeleryInstrumentor().uninstrument()
        HTTPXClientInstrumentor().uninstrument()
