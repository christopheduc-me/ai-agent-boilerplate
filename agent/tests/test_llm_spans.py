"""LLM observability spans (ADR-029 amendment): each adapter call opens a span
tagged with the OpenTelemetry GenAI conventions and the decision outcome. Driven
with an in-memory exporter and a fake chat model — no provider, no paid call."""

import pytest
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

from aiagent.adapters.llm import (
    ActionReply,
    CritiqueReply,
    EnrichmentReply,
    LlmAgentPolicy,
    LlmHitEnricher,
    LlmResultCritic,
)
from aiagent.domain.models import RawSearchHit


class FakeChat:
    """Structured-output fake carrying usage_metadata, so the span records real
    token counts. `raw` quacks like an AIMessage (has .content + usage_metadata)."""

    def __init__(self, parsed: object, input_tokens: int = 11, output_tokens: int = 5) -> None:
        self.parsed = parsed
        self.content = ""
        self.usage_metadata = {"input_tokens": input_tokens, "output_tokens": output_tokens}

    def with_structured_output(self, schema: object, include_raw: bool = False) -> "FakeChat":
        return self

    def invoke(self, prompt: object) -> dict:
        return {"raw": self, "parsed": self.parsed}

    def batch(self, prompts: list, config: dict | None = None) -> list[dict]:
        return [self.invoke(p) for p in prompts]


@pytest.fixture(scope="module")
def exporter() -> InMemorySpanExporter:
    exp = InMemorySpanExporter()
    provider = TracerProvider()
    provider.add_span_processor(SimpleSpanProcessor(exp))
    # First (and only) provider set by the suite; the module tracer's proxy
    # resolves to it lazily, so spans created afterwards are captured.
    trace.set_tracer_provider(provider)
    return exp


def _attrs(exporter: InMemorySpanExporter, name: str) -> dict:
    spans = [s for s in exporter.get_finished_spans() if s.name == name]
    assert len(spans) == 1, f"expected one {name!r} span, got {len(spans)}"
    return dict(spans[0].attributes or {})


def a_hit() -> RawSearchHit:
    return RawSearchHit(title="T", url="https://t", snippet="s", published_at=None)


def test_decide_span_carries_genai_usage_and_the_action(exporter: InMemorySpanExporter) -> None:
    exporter.clear()
    policy = LlmAgentPolicy(
        FakeChat(ActionReply(action="search", query="q", reason="r")),  # type: ignore[arg-type]
        model="claude-opus-4-8",
        system="anthropic",
    )
    policy.decide("goal", [], [])

    attrs = _attrs(exporter, "llm decide")
    assert attrs["gen_ai.operation.name"] == "decide"
    assert attrs["gen_ai.system"] == "anthropic"
    assert attrs["gen_ai.request.model"] == "claude-opus-4-8"
    assert attrs["gen_ai.usage.input_tokens"] == 11
    assert attrs["gen_ai.usage.output_tokens"] == 5
    assert attrs["aiagent.agent.action"] == "search"


def test_critique_span_records_drops_and_gap(exporter: InMemorySpanExporter) -> None:
    exporter.clear()
    critic = LlmResultCritic(
        FakeChat(  # type: ignore[arg-type]
            CritiqueReply(assessment="ok", irrelevant_urls=["https://x"], gap_query="fill this")
        ),
    )
    critic.critique("goal", [a_hit()])

    attrs = _attrs(exporter, "llm critique")
    assert attrs["aiagent.critic.dropped"] == 1
    assert attrs["aiagent.critic.has_gap"] is True


def test_enrich_span_sums_batch_usage(exporter: InMemorySpanExporter) -> None:
    exporter.clear()
    enricher = LlmHitEnricher(
        FakeChat(EnrichmentReply(summary="s"), input_tokens=4, output_tokens=2),  # type: ignore[arg-type]
    )
    enricher.enrich_many([a_hit(), a_hit(), a_hit()])

    attrs = _attrs(exporter, "llm enrich")
    assert attrs["aiagent.llm.batch_size"] == 3
    assert attrs["gen_ai.usage.input_tokens"] == 12  # 3 hits * 4
    assert attrs["gen_ai.usage.output_tokens"] == 6  # 3 hits * 2
