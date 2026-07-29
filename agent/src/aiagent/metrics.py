"""OpenTelemetry metric instruments for the agent (ADR-050) — the aggregate
view the per-run traces (ADR-029) cannot give: token throughput, LLM latency,
spend and job outcomes across the whole fleet, for dashboards and alerts.

Instruments are created from a proxy meter and are **no-ops until a
MeterProvider is installed** (telemetry.configure_telemetry, gated on
OTEL_EXPORTER_OTLP_ENDPOINT). So recording costs nothing in the keyless demo/CI
and flows to the OTLP collector only when telemetry is enabled — exactly like
the spans. Recording lives in the adapters/tasks; the domain stays untouched.
"""

from opentelemetry import metrics

_meter = metrics.get_meter("aiagent")

_llm_tokens = _meter.create_counter(
    "aiagent.llm.tokens",
    unit="{token}",
    description="LLM tokens consumed, by operation / type (input|output) / model backend",
)
_llm_call_duration = _meter.create_histogram(
    "aiagent.llm.call.duration",
    unit="s",
    description="LLM call latency by operation / model backend",
)
_job_cost_usd = _meter.create_counter(
    "aiagent.job.cost",
    unit="USD",
    description="Indicative spend attributed to finished jobs, by outcome",
)
_jobs = _meter.create_counter(
    "aiagent.jobs",
    unit="{job}",
    description="Finished jobs by outcome (completed|failed|paused)",
)


def record_llm_call(
    operation: str, system: str, duration_s: float, input_tokens: int, output_tokens: int
) -> None:
    """One LLM call: its latency (histogram) and token usage (counter)."""
    attrs: dict[str, str] = {"operation": operation}
    if system:
        attrs["gen_ai.system"] = system
    _llm_call_duration.record(duration_s, attrs)
    if input_tokens:
        _llm_tokens.add(input_tokens, {**attrs, "type": "input"})
    if output_tokens:
        _llm_tokens.add(output_tokens, {**attrs, "type": "output"})


def record_job(outcome: str, cost_usd: float) -> None:
    """One finished job: its outcome (counter) and attributed spend (counter)."""
    _jobs.add(1, {"outcome": outcome})
    if cost_usd:
        _job_cost_usd.add(cost_usd, {"outcome": outcome})
