"""LLM adapters: the HitEnricher (ADR-010/011/027), the AgentPolicy driving
the agentic loop (ADR-030) and the ResultCritic (ADR-031). Prompts, reply
handling and usage metering are provider-agnostic: the chat model itself
(Anthropic API or a local Ollama — ADR-041) is injected, built by
`chat_model.make_chat_model`.

Replies use **native structured output** (ADR-043): each adapter binds a
pydantic reply schema via `with_structured_output` — tool calling on
Anthropic, grammar-constrained `json_schema` on Ollama — and converts the
validated reply into the domain type. When the native path yields nothing
(a model that ignored the tool, a validation failure), the raw text goes
through the legacy defensive parsers: the reply degrades, the job never
crashes.
"""

import json
import time
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import datetime
from typing import TYPE_CHECKING, Any

from opentelemetry import trace
from opentelemetry.trace import Span
from pydantic import BaseModel, Field

from aiagent import metrics
from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AskAction,
    Critique,
    EventType,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
    as_utc,
)
from aiagent.domain.usage import UsageMeter

if TYPE_CHECKING:
    from langchain_core.language_models import BaseChatModel, LanguageModelInput


def usage_tokens(response: object) -> tuple[int, int]:
    """(input, output) token counts from langchain's usage_metadata; (0, 0)
    when absent (fake replies, older providers)."""
    usage = getattr(response, "usage_metadata", None) or {}
    return int(usage.get("input_tokens", 0)), int(usage.get("output_tokens", 0))


def record_llm_usage(meter: "UsageMeter | None", response: object) -> None:
    """Reads langchain's usage_metadata (ADR-038); absent metadata still
    counts the call so fake replies and older providers stay visible."""
    if meter is None:
        return
    input_tokens, output_tokens = usage_tokens(response)
    meter.record_llm(input_tokens, output_tokens)


# ---------------------------------------------------------------- tracing (ADR-029 amendment)

# No-op when telemetry is off (the global provider is a proxy) — so the spans
# add nothing to the keyless demo/CI, and appear in Jaeger only when
# OTEL_EXPORTER_OTLP_ENDPOINT is set, as children of the worker's job span.
_tracer = trace.get_tracer("aiagent.llm")


@contextmanager
def llm_span(operation: str, model: str, system: str) -> Iterator[Span]:
    """One span per LLM call, tagged with the OpenTelemetry GenAI conventions
    (`gen_ai.*`). Latency is the span duration; callers add usage and the
    outcome. Model/system are best-effort labels; empty ones are skipped."""
    with _tracer.start_as_current_span(f"llm {operation}") as span:
        span.set_attribute("gen_ai.operation.name", operation)
        if system:
            span.set_attribute("gen_ai.system", system)
        if model:
            span.set_attribute("gen_ai.request.model", model)
        yield span


def record_span_usage(span: Span, input_tokens: int, output_tokens: int) -> None:
    span.set_attribute("gen_ai.usage.input_tokens", input_tokens)
    span.set_attribute("gen_ai.usage.output_tokens", output_tokens)


def structured_with_fallbacks(models: list["BaseChatModel"], schema: type[BaseModel]) -> Any:
    """Binds structured output (ADR-043) on each model and chains them with
    LangChain fallbacks (ADR-052): the primary runs first; if it errors (provider
    down/quota, not a transient blip — those are the ADR-044 retries), the next
    model is tried, in order. A single model means no fallback wrapper, i.e. the
    exact previous behavior."""
    runnables = [model.with_structured_output(schema, include_raw=True) for model in models]
    head, *tail = runnables
    return head.with_fallbacks(tail) if tail else head


def split_structured(result: object) -> tuple[object, object]:
    """Splits an `include_raw=True` structured result into (raw message,
    parsed schema or None). Anything unexpected counts as unparsed."""
    if isinstance(result, dict):
        return result.get("raw"), result.get("parsed")
    return result, None


def raw_text(raw: object) -> str:
    """The raw message's text content, for the fallback parsers."""
    content = getattr(raw, "content", "")
    return content if isinstance(content, str) else str(content)


# ---------------------------------------------------------------- enrichment

ENRICHMENT_PROMPT = """\
You analyze a web search result about a topic and report, through the reply
schema:

- published_date: in ISO 8601 (YYYY-MM-DD or full timestamp), or null if it
  cannot be determined with reasonable confidence — never prose.
- event_type: exactly one of "announcement", "release", "funding", "legal",
  "incident", "research", "opinion", "other" (lowercase).
- summary: one factual sentence (max 25 words) describing the event this
  page reports.

Title: {title}
URL: {url}
Excerpt: {snippet}
"""


class EnrichmentReply(BaseModel):
    """Structured analysis of one web search result (ADR-043). Fields stay
    loosely typed on purpose: an invented event type or a prose date must
    degrade field by field during conversion, never void the whole reply."""

    published_date: str | None = Field(
        default=None,
        description=(
            "The publication date in ISO 8601 (YYYY-MM-DD or full timestamp), "
            "or null if it cannot be determined with reasonable confidence"
        ),
    )
    event_type: str | None = Field(
        default=None,
        description=(
            'One of "announcement", "release", "funding", "legal", "incident", '
            '"research", "opinion", "other"'
        ),
    )
    summary: str | None = Field(
        default=None,
        description="One factual sentence (max 25 words) describing the event this page reports",
    )


def parse_extracted_date(text: str) -> datetime | None:
    """Parses an ISO date; anything that is not a clean ISO date means unknown."""
    cleaned = text.strip().strip("`\"' ")
    if not cleaned or cleaned.lower() in ("unknown", "null", "none"):
        return None
    try:
        return as_utc(datetime.fromisoformat(cleaned.replace("Z", "+00:00")))
    except ValueError:
        return None


def enrichment_from_reply(reply: EnrichmentReply) -> HitEnrichment:
    """Converts the validated reply, degrading field by field (ADR-027).
    Case-tolerant on the event type: schema-mode models capitalize more
    freely than prompt-following ones."""
    published_at = parse_extracted_date(reply.published_date) if reply.published_date else None
    try:
        event_type = EventType(str(reply.event_type).strip().lower())
    except ValueError:
        event_type = EventType.OTHER
    summary = reply.summary.strip() if reply.summary and reply.summary.strip() else None
    return HitEnrichment(published_at=published_at, event_type=event_type, summary=summary)


def parse_enrichment(text: str) -> HitEnrichment:
    """Fallback parser (ADR-043) for models that answered in text: any
    malformed piece degrades to its neutral value instead of failing the job."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        cleaned = cleaned.removeprefix("json").strip()
    try:
        payload = json.loads(cleaned)
    except ValueError:
        return HitEnrichment()
    if not isinstance(payload, dict):
        return HitEnrichment()

    published_at = None
    if isinstance(payload.get("published_date"), str):
        published_at = parse_extracted_date(payload["published_date"])
    try:
        event_type = EventType(str(payload.get("event_type")))
    except ValueError:
        event_type = EventType.OTHER
    summary = payload.get("summary")
    if not isinstance(summary, str) or not summary.strip():
        summary = None

    return HitEnrichment(published_at=published_at, event_type=event_type, summary=summary)


class LlmHitEnricher:
    """Live adapter — the model call itself is never exercised in CI
    (ADR-012). `llm` is injectable so the prompt/convert/fallback logic
    around it stays unit-testable with a fake chat model."""

    def __init__(
        self,
        llm: "BaseChatModel",
        meter: UsageMeter | None = None,
        concurrency: int = 5,
        model: str = "",
        system: str = "",
        fallbacks: "list[BaseChatModel] | None" = None,
    ) -> None:
        self._meter = meter
        self._structured = structured_with_fallbacks([llm, *(fallbacks or [])], EnrichmentReply)
        # Bounds the parallel per-hit calls (ADR-042): fast, without letting a
        # burst of hits hammer the provider (or overload a local Ollama).
        self._concurrency = concurrency
        self._model = model
        self._system = system

    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]:
        if not hits:
            return []
        prompts: list[Any] = [
            ENRICHMENT_PROMPT.format(title=hit.title, url=hit.url, snippet=hit.snippet)
            for hit in hits
        ]
        # One span for the whole batch (ADR-042 runs the per-hit calls together,
        # so there is no honest per-hit latency to record).
        with llm_span("enrich", self._model, self._system) as span:
            span.set_attribute("aiagent.llm.batch_size", len(hits))
            # One langchain batch = the per-hit calls run concurrently under the
            # hood; replies come back in prompt order. Usage is recorded here, on
            # the caller's thread — the meter needs no thread-safety.
            start = time.perf_counter()
            results = self._structured.batch(prompts, config={"max_concurrency": self._concurrency})
            duration = time.perf_counter() - start
            enrichments = []
            input_tokens = output_tokens = 0
            for result in results:
                raw, parsed = split_structured(result)
                record_llm_usage(self._meter, raw)
                got_in, got_out = usage_tokens(raw)
                input_tokens += got_in
                output_tokens += got_out
                if isinstance(parsed, EnrichmentReply):
                    enrichments.append(enrichment_from_reply(parsed))
                else:
                    enrichments.append(parse_enrichment(raw_text(raw)))
            record_span_usage(span, input_tokens, output_tokens)
            metrics.record_llm_call("enrich", self._system, duration, input_tokens, output_tokens)
            return enrichments

    def enrich(self, hit: RawSearchHit) -> HitEnrichment:
        """Single-hit convenience (live drift tests, ADR-012)."""
        return self.enrich_many([hit])[0]


# ---------------------------------------------------------------- policy

POLICY_PROMPT = """\
You are a research agent gathering fresh, relevant web results about a goal.
Decide your next action through the reply schema: search (again), finish, or
— only if the goal is genuinely ambiguous AND no clarification is present
below — ask the user ONE short question before searching.

Rules: refine or vary the query instead of repeating one that brought nothing
new; stop as soon as coverage looks sufficient or further searches stop adding
results. Never ask once a clarification is present. The reason is one short
sentence, shown to the user as your journal.

Goal: {goal}

Searches so far (query -> new results added):
{transcript}

Results collected so far ({count}):
{titles}
"""


class ActionReply(BaseModel):
    """The policy's next action (ADR-030/043)."""

    action: str = Field(
        description='"search" to search again, "finish" to stop, '
        '"ask" to ask the user one clarifying question'
    )
    reason: str | None = Field(
        default=None,
        description="One short sentence explaining the decision, shown to the user",
    )
    query: str | None = Field(
        default=None,
        description='The search query — required when action is "search"',
    )
    question: str | None = Field(
        default=None,
        description='The clarifying question — required when action is "ask"',
    )


def action_from_reply(reply: ActionReply) -> AgentAction:
    """Converts the validated reply; a search without a query (or an ask
    without a question) degrades to FINISH — never a crash, never a burned
    budget (ADR-030)."""
    reason = reply.reason.strip() if reply.reason and reply.reason.strip() else "no reason given"
    action = reply.action.strip().lower()
    if action == "search" and reply.query and reply.query.strip():
        return SearchAction(query=reply.query.strip(), reason=reason)
    if action == "ask" and reply.question and reply.question.strip():
        return AskAction(question=reply.question.strip(), reason=reason)
    return FinishAction(reason=reason)


def _action_label(action: AgentAction) -> str:
    if isinstance(action, SearchAction):
        return "search"
    if isinstance(action, AskAction):
        return "ask"
    return "finish"


def parse_action(text: str) -> AgentAction:
    """Fallback parser (ADR-043): anything malformed means FINISH — a
    confused model must never burn the step budget."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        cleaned = cleaned.removeprefix("json").strip()
    try:
        payload = json.loads(cleaned)
    except ValueError:
        return FinishAction(reason="policy reply was not valid JSON")
    if not isinstance(payload, dict):
        return FinishAction(reason="policy reply was not a JSON object")

    reason = payload.get("reason")
    reason = reason.strip() if isinstance(reason, str) and reason.strip() else "no reason given"
    query = payload.get("query")
    if payload.get("action") == "search" and isinstance(query, str) and query.strip():
        return SearchAction(query=query.strip(), reason=reason)
    question = payload.get("question")
    if payload.get("action") == "ask" and isinstance(question, str) and question.strip():
        return AskAction(question=question.strip(), reason=reason)
    return FinishAction(reason=reason)


class LlmAgentPolicy:
    """Live AgentPolicy (ADR-030) — the LLM sees the goal, the transcript of
    its own past decisions and the collected titles (token-frugal), and picks
    the next action. Same injectable-`llm` pattern as LlmHitEnricher."""

    def __init__(
        self,
        llm: "BaseChatModel",
        meter: UsageMeter | None = None,
        model: str = "",
        system: str = "",
        fallbacks: "list[BaseChatModel] | None" = None,
    ) -> None:
        self._meter = meter
        self._structured = structured_with_fallbacks([llm, *(fallbacks or [])], ActionReply)
        self._model = model
        self._system = system

    def decide(self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]) -> AgentAction:
        transcript = "\n".join(f'- "{s.detail}" -> {s.new_hits} new' for s in steps) or "- none yet"
        titles = "\n".join(f"- {h.title}" for h in hits[:30]) or "- none yet"
        prompt: LanguageModelInput = POLICY_PROMPT.format(
            goal=goal, transcript=transcript, count=len(hits), titles=titles
        )
        with llm_span("decide", self._model, self._system) as span:
            start = time.perf_counter()
            raw, parsed = split_structured(self._structured.invoke(prompt))
            duration = time.perf_counter() - start
            record_llm_usage(self._meter, raw)
            input_tokens, output_tokens = usage_tokens(raw)
            record_span_usage(span, input_tokens, output_tokens)
            metrics.record_llm_call("decide", self._system, duration, input_tokens, output_tokens)
            action = (
                action_from_reply(parsed)
                if isinstance(parsed, ActionReply)
                else parse_action(raw_text(raw))
            )
            # The decision is the single most useful attribute for reading a run.
            span.set_attribute("aiagent.agent.action", _action_label(action))
            return action


# ---------------------------------------------------------------- critique

CRITIQUE_PROMPT = """\
You are reviewing the results a research agent collected for a goal, before
they are delivered. Through the reply schema: judge how well the results
cover the goal, flag the results clearly unrelated to it (be conservative —
only obvious noise), and if one important angle is missing, name the single
search query that would fill it.

Goal: {goal}

Results ({count}):
{listing}
"""


class CritiqueReply(BaseModel):
    """The critic's verdict (ADR-031/043)."""

    assessment: str | None = Field(
        default=None,
        description=(
            "One or two sentences judging how well the results cover the goal "
            "(shown to the user verbatim)"
        ),
    )
    irrelevant_urls: list[str] = Field(
        default_factory=list,
        description=(
            "The URLs of results clearly unrelated to the goal (empty list if "
            "none). Be conservative: only drop obvious noise"
        ),
    )
    gap_query: str | None = Field(
        default=None,
        description=(
            "If one important angle is missing, a single search query that "
            "would fill it; otherwise null"
        ),
    )


def critique_from_reply(reply: CritiqueReply) -> Critique:
    """Converts the validated reply, defaulting each missing piece (ADR-031)."""
    assessment = (
        reply.assessment.strip()
        if reply.assessment and reply.assessment.strip()
        else "no assessment given"
    )
    gap = reply.gap_query.strip() if reply.gap_query and reply.gap_query.strip() else None
    return Critique(
        assessment=assessment,
        irrelevant_urls=tuple(u for u in reply.irrelevant_urls if isinstance(u, str)),
        gap_query=gap,
    )


def parse_critique(text: str) -> Critique:
    """Fallback parser (ADR-043): anything malformed becomes a neutral
    critique (no drops, no gap) — the review must never fail a job."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        cleaned = cleaned.removeprefix("json").strip()
    try:
        payload = json.loads(cleaned)
    except ValueError:
        return Critique(assessment="self-critique unavailable (reply was not valid JSON)")
    if not isinstance(payload, dict):
        return Critique(assessment="self-critique unavailable (reply was not a JSON object)")

    assessment = payload.get("assessment")
    if not isinstance(assessment, str) or not assessment.strip():
        assessment = "no assessment given"
    urls = payload.get("irrelevant_urls")
    irrelevant = tuple(u for u in urls if isinstance(u, str)) if isinstance(urls, list) else ()
    gap = payload.get("gap_query")
    gap_query = gap.strip() if isinstance(gap, str) and gap.strip() else None
    return Critique(assessment=assessment.strip(), irrelevant_urls=irrelevant, gap_query=gap_query)


class LlmResultCritic:
    """Live ResultCritic (ADR-031) — one call reviewing the whole result set;
    same injectable-`llm` pattern as the other adapters."""

    def __init__(
        self,
        llm: "BaseChatModel",
        meter: UsageMeter | None = None,
        model: str = "",
        system: str = "",
        fallbacks: "list[BaseChatModel] | None" = None,
    ) -> None:
        self._meter = meter
        self._structured = structured_with_fallbacks([llm, *(fallbacks or [])], CritiqueReply)
        self._model = model
        self._system = system

    def critique(self, goal: str, hits: list[RawSearchHit]) -> Critique:
        listing = "\n".join(f"- {h.title} — {h.url}\n  {h.snippet}" for h in hits[:30]) or "- none"
        prompt: LanguageModelInput = CRITIQUE_PROMPT.format(
            goal=goal, count=len(hits), listing=listing
        )
        with llm_span("critique", self._model, self._system) as span:
            start = time.perf_counter()
            raw, parsed = split_structured(self._structured.invoke(prompt))
            duration = time.perf_counter() - start
            record_llm_usage(self._meter, raw)
            input_tokens, output_tokens = usage_tokens(raw)
            record_span_usage(span, input_tokens, output_tokens)
            metrics.record_llm_call("critique", self._system, duration, input_tokens, output_tokens)
            critique = (
                critique_from_reply(parsed)
                if isinstance(parsed, CritiqueReply)
                else parse_critique(raw_text(raw))
            )
            span.set_attribute("aiagent.critic.dropped", len(critique.irrelevant_urls))
            span.set_attribute("aiagent.critic.has_gap", critique.gap_query is not None)
            return critique
