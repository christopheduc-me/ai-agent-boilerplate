import json
from datetime import UTC, datetime

import httpx
import pytest
import respx

from aiagent.adapters.llm import (
    ENRICHMENT_PROMPT,
    ActionReply,
    CritiqueReply,
    EnrichmentReply,
    LlmAgentPolicy,
    LlmHitEnricher,
    LlmResultCritic,
    action_from_reply,
    enrichment_from_reply,
    parse_action,
    parse_critique,
    parse_enrichment,
    parse_extracted_date,
)
from aiagent.adapters.sink import HttpResultSink, serialize_result
from aiagent.adapters.tavily import hit_from_tavily, hits_from_tavily_response
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
    AskAction,
    Critique,
    DateConfidence,
    EventType,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    ResearchResult,
    SearchAction,
)

# ---------------------------------------------------------------- tavily mapping


def test_tavily_item_with_published_date() -> None:
    item = {
        "title": "T",
        "url": "https://t",
        "content": "excerpt",
        "published_date": "2025-04-02T10:00:00Z",
    }
    hit = hit_from_tavily(item)
    assert hit.published_at == datetime(2025, 4, 2, 10, 0, tzinfo=UTC)
    assert hit.raw == item


def test_tavily_item_without_or_with_garbage_date() -> None:
    assert hit_from_tavily({"title": "T", "url": "https://t"}).published_at is None
    assert (
        hit_from_tavily({"title": "T", "url": "https://t", "published_date": "yesterday"})
    ).published_at is None


def test_tavily_response_maps_results() -> None:
    hits = hits_from_tavily_response({"results": [{"title": "T", "url": "https://t"}]})
    assert [h.url for h in hits] == ["https://t"]


def test_tavily_error_response_raises_instead_of_returning_empty() -> None:
    # A quota/key error must fail the job, not masquerade as zero results — else
    # the agent burns its step budget searching against a dead provider.
    with pytest.raises(RuntimeError, match="Tavily search failed"):
        hits_from_tavily_response({"error": "Error 432: usage limit exceeded"})


def test_tavily_unexpected_shape_is_empty_not_a_crash() -> None:
    assert hits_from_tavily_response("unexpected") == []
    assert hits_from_tavily_response({}) == []


def test_ddg_item_maps_to_a_hit_without_a_date() -> None:
    from aiagent.adapters.duckduckgo import hit_from_ddg

    hit = hit_from_ddg({"title": "T", "href": "https://d", "body": "excerpt"})
    assert (hit.title, hit.url, hit.snippet) == ("T", "https://d", "excerpt")
    assert hit.published_at is None


# ---------------------------------------------------------------- llm reply parsing


def test_parse_extracted_date_accepts_iso_formats() -> None:
    assert parse_extracted_date("2024-05-01") == datetime(2024, 5, 1, tzinfo=UTC)
    assert parse_extracted_date(" `2024-05-01T10:30:00Z` ") == datetime(
        2024, 5, 1, 10, 30, tzinfo=UTC
    )


def test_parse_extracted_date_rejects_unknown_and_prose() -> None:
    assert parse_extracted_date("unknown") is None
    assert parse_extracted_date("The article was published in May 2024.") is None
    assert parse_extracted_date("") is None


# ---------------------------------------------------------------- enrichment parsing


def test_parse_enrichment_full_payload() -> None:
    enrichment = parse_enrichment(
        '{"published_date": "2026-03-01", "event_type": "release", "summary": "v2 is out."}'
    )
    assert enrichment.published_at == datetime(2026, 3, 1, tzinfo=UTC)
    assert enrichment.event_type == EventType.RELEASE
    assert enrichment.summary == "v2 is out."


def test_parse_enrichment_tolerates_code_fences_and_nulls() -> None:
    enrichment = parse_enrichment(
        '```json\n{"published_date": null, "event_type": "legal", "summary": "A ruling."}\n```'
    )
    assert enrichment.published_at is None
    assert enrichment.event_type == EventType.LEGAL


def test_parse_enrichment_degrades_gracefully() -> None:
    # Prose, bad enum, blank summary: neutral values, never an exception.
    assert parse_enrichment("not json at all") == HitEnrichment()
    assert parse_enrichment('["a", "list"]') == HitEnrichment()
    degraded = parse_enrichment(
        '{"published_date": "someday", "event_type": "party", "summary": "  "}'
    )
    assert degraded.published_at is None
    assert degraded.event_type == EventType.OTHER
    assert degraded.summary is None


# ---------------------------------------------------------------- LlmHitEnricher


class FakeChatModel:
    """Stands in for the chat model in structured-output mode (ADR-043):
    records the prompt, replies with `parsed` (the validated schema instance —
    the happy path) or with `parsed=None` and raw `content` (native structured
    output failed → the adapter must fall back to text parsing). Prompt
    building, conversion and fallback parsing run for real (ADR-012)."""

    def __init__(self, content: object = "", parsed: object = None) -> None:
        self.content = content
        self.parsed = parsed
        self.prompts: list[str] = []
        self.batch_configs: list[dict | None] = []
        self.schema: object = None

    def with_structured_output(self, schema: object, include_raw: bool = False) -> "FakeChatModel":
        assert include_raw, "adapters must keep the raw message (usage metering, ADR-038)"
        self.schema = schema
        return self

    def invoke(self, prompt: str) -> dict:
        self.prompts.append(prompt)
        # `raw` quacks like an AIMessage: has .content and no usage_metadata.
        return {"raw": self, "parsed": self.parsed}

    def batch(self, prompts: list[str], config: dict | None = None) -> list[dict]:
        self.batch_configs.append(config)
        return [self.invoke(p) for p in prompts]


def a_hit(title: str = "T") -> RawSearchHit:
    return RawSearchHit(title=title, url="https://t", snippet="an excerpt", published_at=None)


def test_llm_enricher_converts_the_structured_reply() -> None:
    # ADR-043 happy path: the model filled the native schema.
    llm = FakeChatModel(
        parsed=EnrichmentReply(published_date="2026-03-01", event_type="funding", summary="$10M.")
    )
    enricher = LlmHitEnricher(llm)  # type: ignore[arg-type]

    enrichment = enricher.enrich(a_hit(title="My Article"))

    assert enrichment.published_at == datetime(2026, 3, 1, tzinfo=UTC)
    assert enrichment.event_type == EventType.FUNDING
    assert enrichment.summary == "$10M."
    assert llm.schema is EnrichmentReply
    assert llm.prompts[0] == ENRICHMENT_PROMPT.format(
        title="My Article", url="https://t", snippet="an excerpt"
    )


def test_llm_enricher_falls_back_to_text_parsing_when_structuring_failed() -> None:
    # ADR-043 degradation: parsed=None but the raw content happens to be JSON
    # (a model that ignored the tool but still answered) — nothing is lost.
    llm = FakeChatModel(
        content='{"published_date": "2026-03-01", "event_type": "funding", "summary": "$10M."}'
    )
    enrichment = LlmHitEnricher(llm).enrich(a_hit())  # type: ignore[arg-type]

    assert enrichment.published_at == datetime(2026, 3, 1, tzinfo=UTC)
    assert enrichment.event_type == EventType.FUNDING


def test_enrichment_reply_degrades_field_by_field() -> None:
    # An invented event type or a prose date must not void the whole reply.
    enrichment = enrichment_from_reply(
        EnrichmentReply(published_date="last spring", event_type="product-launch", summary=" S. ")
    )
    assert enrichment.published_at is None
    assert enrichment.event_type == EventType.OTHER
    assert enrichment.summary == "S."


def test_enrichment_reply_is_case_tolerant_on_the_event_type() -> None:
    # Schema-mode models capitalize the enum freely ("Release", "RESEARCH") —
    # found live with gemma4 returning "Software Release". Must not fall to OTHER.
    assert enrichment_from_reply(EnrichmentReply(event_type="Release")).event_type == (
        EventType.RELEASE
    )
    assert enrichment_from_reply(EnrichmentReply(event_type="RESEARCH")).event_type == (
        EventType.RESEARCH
    )


def test_llm_enricher_batches_hits_through_one_bounded_batch_call() -> None:
    # ADR-042: one llm.batch per result set (concurrent under the hood),
    # bounded so a burst of hits cannot hammer the provider.
    llm = FakeChatModel(parsed=EnrichmentReply(event_type="release", summary="S."))
    enricher = LlmHitEnricher(llm)  # type: ignore[arg-type]

    enrichments = enricher.enrich_many([a_hit(title="A"), a_hit(title="B"), a_hit(title="C")])

    assert [e.event_type for e in enrichments] == [EventType.RELEASE] * 3
    assert len(llm.batch_configs) == 1  # one batch, not three invokes
    assert llm.batch_configs[0] == {"max_concurrency": 5}
    assert [f"Title: {t}" in p for t, p in zip(["A", "B", "C"], llm.prompts, strict=True)] == [
        True,
        True,
        True,
    ]


def test_llm_enricher_meters_every_call_of_the_batch() -> None:
    from aiagent.domain.usage import UsageMeter

    llm = FakeChatModel(content="{}")
    meter = UsageMeter()
    LlmHitEnricher(llm, meter=meter).enrich_many([a_hit(), a_hit()])  # type: ignore[arg-type]

    assert meter.snapshot().llm_calls == 2


def test_llm_enricher_returns_no_enrichment_for_no_hits() -> None:
    llm = FakeChatModel(content="{}")
    assert LlmHitEnricher(llm).enrich_many([]) == []  # type: ignore[arg-type]
    assert llm.batch_configs == []  # no pointless provider round-trip


def test_llm_enricher_tolerates_structured_content_blocks() -> None:
    # Some models return content as a list of blocks; the adapter must not
    # crash — non-JSON stringification degrades to a neutral enrichment.
    blocks = [{"type": "text", "text": "{}"}]
    enricher = LlmHitEnricher(FakeChatModel(content=blocks))  # type: ignore[arg-type]
    assert enricher.enrich(a_hit()) == HitEnrichment()


# ---------------------------------------------------------------- http sink


def a_result() -> ResearchResult:
    return ResearchResult(
        title="T",
        url="https://t",
        snippet="s",
        published_at=datetime(2025, 4, 2, tzinfo=UTC),
        date_confidence=DateConfidence.HIGH,
        raw={"k": "v"},
    )


@respx.mock
def test_sink_delivers_serialized_results_with_internal_token() -> None:
    route = respx.post("http://backend:8000/internal/jobs/job-1/results").mock(
        return_value=httpx.Response(204)
    )
    sink = HttpResultSink("http://backend:8000", "secret-token")

    sink.deliver("job-1", [a_result()])

    request = route.calls.last.request
    assert request.headers["x-internal-token"] == "secret-token"
    payload = json.loads(request.content)
    assert payload["results"] == [
        {
            "title": "T",
            "url": "https://t",
            "snippet": "s",
            "published_at": "2025-04-02T00:00:00+00:00",
            "date_confidence": "high",
            "event_type": "other",
            "summary": None,
            "is_new": True,
            "raw": {"k": "v"},
        }
    ]


@respx.mock
def test_sink_marks_the_job_started() -> None:
    route = respx.post("http://backend:8000/internal/jobs/job-1/started").mock(
        return_value=httpx.Response(204)
    )
    sink = HttpResultSink("http://backend:8000", "secret-token")

    sink.mark_started("job-1")

    assert route.calls.last.request.headers["x-internal-token"] == "secret-token"


@respx.mock
def test_sink_propagates_the_correlation_id() -> None:
    route = respx.post("http://backend:8000/internal/jobs/job-1/started").mock(
        return_value=httpx.Response(204)
    )
    sink = HttpResultSink("http://backend:8000", "secret-token", request_id="corr-42")

    sink.mark_started("job-1")

    assert route.calls.last.request.headers["x-request-id"] == "corr-42"


@respx.mock
def test_sink_reports_failures() -> None:
    route = respx.post("http://backend:8000/internal/jobs/job-1/failure").mock(
        return_value=httpx.Response(204)
    )
    sink = HttpResultSink("http://backend:8000", "secret-token")

    sink.report_failure("job-1", "boom")

    assert json.loads(route.calls.last.request.content) == {"error": "boom"}


def test_serialize_result_none_date() -> None:
    result = ResearchResult(
        title="T",
        url="https://t",
        snippet="",
        published_at=None,
        date_confidence=DateConfidence.UNKNOWN,
    )
    assert serialize_result(result)["published_at"] is None


# ---------------------------------------------------------------- agent policy (ADR-030)


def test_parse_action_search_and_finish() -> None:
    assert parse_action('{"action": "search", "query": "rust 2026", "reason": "refine"}') == (
        SearchAction(query="rust 2026", reason="refine")
    )
    assert parse_action('{"action": "finish", "reason": "coverage ok"}') == (
        FinishAction(reason="coverage ok")
    )


def test_parse_action_degrades_to_finish() -> None:
    # Anything malformed must stop the loop, never crash or burn budget.
    assert isinstance(parse_action("I think I should search more"), FinishAction)
    assert isinstance(parse_action('{"action": "search"}'), FinishAction)  # no query
    assert isinstance(parse_action('{"action": "search", "query": "  "}'), FinishAction)
    assert isinstance(parse_action('["search"]'), FinishAction)


def test_parse_action_tolerates_code_fences() -> None:
    fenced = '```json\n{"action": "search", "query": "q", "reason": "r"}\n```'
    assert parse_action(fenced) == SearchAction(query="q", reason="r")


def test_llm_policy_shows_the_transcript_and_converts_the_decision() -> None:
    llm = FakeChatModel(parsed=ActionReply(action="search", query="rust news", reason="start"))
    policy = LlmAgentPolicy(llm)  # type: ignore[arg-type]
    steps = [AgentStep(seq=1, kind=AgentStepKind.SEARCH, detail="rust", reason="r", new_hits=2)]

    action = policy.decide("rust", steps, [a_hit(title="Rust 1.99 released")])

    assert action == SearchAction(query="rust news", reason="start")
    prompt = llm.prompts[0]
    assert "Goal: rust" in prompt
    assert '- "rust" -> 2 new' in prompt
    assert "- Rust 1.99 released" in prompt


def test_llm_policy_falls_back_to_text_parsing_when_structuring_failed() -> None:
    llm = FakeChatModel(content='{"action": "search", "query": "rust news", "reason": "start"}')
    action = LlmAgentPolicy(llm).decide("rust", [], [])  # type: ignore[arg-type]
    assert action == SearchAction(query="rust news", reason="start")


def test_action_reply_without_its_required_detail_degrades_to_finish() -> None:
    # A search without a query or an ask without a question must never crash
    # the loop — same guarantee as the text parser (ADR-030).
    assert isinstance(action_from_reply(ActionReply(action="search", reason="r")), FinishAction)
    assert isinstance(action_from_reply(ActionReply(action="ask", reason="r")), FinishAction)
    ask = action_from_reply(ActionReply(action="ask", question="Which one?", reason="r"))
    assert isinstance(ask, AskAction) and ask.question == "Which one?"


def test_action_reply_is_case_tolerant_on_the_action() -> None:
    # Schema-mode models may capitalize the action ("Search") — must still run.
    got = action_from_reply(ActionReply(action="Search", query="rust", reason="go"))
    assert isinstance(got, SearchAction) and got.query == "rust"


@respx.mock
def test_sink_reports_agent_steps() -> None:
    route = respx.post("http://backend:8000/internal/jobs/job-1/steps").mock(
        return_value=httpx.Response(204)
    )
    sink = HttpResultSink("http://backend:8000", "secret")
    step = AgentStep(seq=1, kind=AgentStepKind.SEARCH, detail="rust", reason="start", new_hits=4)

    sink.report_step("job-1", step)

    import json as _json

    assert _json.loads(route.calls.last.request.content) == {
        "seq": 1,
        "kind": "search",
        "detail": "rust",
        "reason": "start",
        "new_hits": 4,
    }


# ---------------------------------------------------------------- self-critique (ADR-031)


def test_parse_critique_full_payload() -> None:
    reply = (
        '{"assessment": "Good coverage.", '
        '"irrelevant_urls": ["https://spam", 42], "gap_query": " q recent "}'
    )
    critique = parse_critique(reply)
    assert critique.assessment == "Good coverage."
    assert critique.irrelevant_urls == ("https://spam",)  # non-strings ignored
    assert critique.gap_query == "q recent"


def test_parse_critique_tolerates_fences_and_nulls() -> None:
    fenced = '```json\n{"assessment": "ok", "irrelevant_urls": [], "gap_query": null}\n```'
    assert parse_critique(fenced) == Critique(assessment="ok")


def test_parse_critique_degrades_to_a_neutral_review() -> None:
    for bad in ("prose, not JSON", "[1, 2]", '{"irrelevant_urls": "not-a-list"}'):
        critique = parse_critique(bad)
        assert critique.irrelevant_urls == () and critique.gap_query is None


def test_llm_critic_lists_the_results_and_converts_the_verdict() -> None:
    llm = FakeChatModel(parsed=CritiqueReply(assessment="One gap.", gap_query="rust 2026"))
    critic = LlmResultCritic(llm)  # type: ignore[arg-type]

    critique = critic.critique("rust", [a_hit(title="Rust 1.99 released")])

    assert critique == Critique(assessment="One gap.", gap_query="rust 2026")
    prompt = llm.prompts[0]
    assert "Goal: rust" in prompt and "- Rust 1.99 released" in prompt


def test_llm_critic_falls_back_to_text_parsing_when_structuring_failed() -> None:
    llm = FakeChatModel(content="prose, definitely not a filled schema")
    critique = LlmResultCritic(llm).critique("rust", [a_hit()])  # type: ignore[arg-type]
    # Neutral degradation (ADR-031): nothing dropped, no gap.
    assert critique.irrelevant_urls == () and critique.gap_query is None


# ---------------------------------------------------------------- clarification (ADR-032)


def test_parse_action_ask() -> None:
    assert parse_action('{"action": "ask", "question": "Animal or car?", "reason": "r"}') == (
        AskAction(question="Animal or car?", reason="r")
    )
    # A blank question is useless: degrade to finish, never burn the pause.
    assert isinstance(parse_action('{"action": "ask", "question": "  "}'), FinishAction)


@respx.mock
def test_sink_requests_clarification() -> None:
    route = respx.post("http://backend:8000/internal/jobs/job-1/question").mock(
        return_value=httpx.Response(204)
    )
    sink = HttpResultSink("http://backend:8000", "secret")

    sink.request_clarification("job-1", "Animal or car?")

    import json as _json

    assert _json.loads(route.calls.last.request.content) == {"question": "Animal or car?"}
