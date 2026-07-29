"""Opt-in OpenTelemetry traces and metrics (ADR-029/050).

Gated on ``OTEL_EXPORTER_OTLP_ENDPOINT`` exactly like the Rust backend: unset
(the default) means nothing is installed and the process behaves as before.
When set, the agent continues the trace started by the backend — FastAPI
extracts the W3C ``traceparent`` sent by the dispatcher, the Celery
instrumentation carries it from producer to worker through the broker, and the
httpx instrumentation propagates it again on the result callbacks. This is the
distributed extension of the ADR-018 correlation ids.

The same gate installs an OTLP **metric** provider (ADR-050): the agent's LLM
adapters and Celery tasks emit counters/histograms (tokens, call latency, cost,
job outcomes) through it. Traces answer "why was this run slow"; metrics answer
"is the fleet healthy / trending". Both push to the same OTLP endpoint — a
collector fans traces to Jaeger and metrics to Prometheus.
"""

import os

from opentelemetry import metrics, trace
from opentelemetry.exporter.otlp.proto.http.metric_exporter import OTLPMetricExporter
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.celery import CeleryInstrumentor
from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor
from opentelemetry.sdk.metrics import MeterProvider
from opentelemetry.sdk.metrics.export import PeriodicExportingMetricReader
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor


def configure_telemetry(service_name: str) -> bool:
    """Installs the OTLP tracer + meter providers plus the Celery and httpx
    instrumentations when ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set (the exporters
    read the standard ``OTEL_*`` variables themselves). Returns whether
    telemetry is enabled; a no-op returning False otherwise."""
    if not os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT"):
        return False

    resource = Resource.create({"service.name": service_name})
    provider = TracerProvider(resource=resource)
    provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))
    trace.set_tracer_provider(provider)

    # Metrics (ADR-050): periodic OTLP push, same endpoint as the spans.
    reader = PeriodicExportingMetricReader(OTLPMetricExporter())
    metrics.set_meter_provider(MeterProvider(resource=resource, metric_readers=[reader]))

    # Idempotent: both instrumentors bail out when already instrumented.
    CeleryInstrumentor().instrument()
    HTTPXClientInstrumentor().instrument()
    return True
