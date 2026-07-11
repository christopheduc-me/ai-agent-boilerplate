"""Opt-in OpenTelemetry traces (ADR-029).

Gated on ``OTEL_EXPORTER_OTLP_ENDPOINT`` exactly like the Rust backend: unset
(the default) means nothing is installed and the process behaves as before.
When set, the agent continues the trace started by the backend — FastAPI
extracts the W3C ``traceparent`` sent by the dispatcher, the Celery
instrumentation carries it from producer to worker through the broker, and the
httpx instrumentation propagates it again on the result callbacks. This is the
distributed extension of the ADR-018 correlation ids.
"""

import os

from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.celery import CeleryInstrumentor
from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor


def configure_telemetry(service_name: str) -> bool:
    """Installs the OTLP tracer provider plus the Celery and httpx
    instrumentations when ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set (the exporter
    reads the standard ``OTEL_*`` variables itself). Returns whether telemetry
    is enabled; a no-op returning False otherwise."""
    if not os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT"):
        return False

    provider = TracerProvider(resource=Resource.create({"service.name": service_name}))
    provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))
    trace.set_tracer_provider(provider)
    # Idempotent: both instrumentors bail out when already instrumented.
    CeleryInstrumentor().instrument()
    HTTPXClientInstrumentor().instrument()
    return True
