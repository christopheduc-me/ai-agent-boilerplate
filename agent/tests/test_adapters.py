import json
from datetime import UTC, datetime

import httpx
import respx

from aiagent.adapters.llm import (
    ENRICHMENT_PROMPT,
    ClaudeAgentPolicy,
    ClaudeHitEnricher,
    ClaudeResultCritic,
    parse_action,
    parse_critique,
    parse_enrichment,
    parse_extracted_date,
)
from aiagent.adapters.sink import HttpResultSink, serialize_result
from aiagent.adapters.tavily import hit_from_tavily
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
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


# ---------------------------------------------------------------- ClaudeHitEnricher


class FakeChatModel:
    """Stands in for ChatAnthropic: records the prompt, replies with `content`.

    The model call is the only thing faked — prompt building and reply parsing
    (the adapter's actual logic) run for real (ADR-012).
    """

    def __init__(self, content: object) -> None:
        self.content = content
        self.prompts: list[str] = []

    def invoke(self, prompt: str) -> "FakeChatModel":
        self.prompts.append(prompt)
        return self  # quacks like an AIMessage: has .content


def a_hit(title: str = "T") -> RawSearchHit:
    return RawSearchHit(title=title, url="https://t", snippet="an excerpt", published_at=None)


def test_claude_enricher_builds_the_prompt_and_parses_the_reply() -> None:
    llm = FakeChatModel(
        content='{"published_date": "2026-03-01", "event_type": "funding", "summary": "$10M."}'
    )
    enricher = ClaudeHitEnricher("claude-opus-4-8", llm=llm)  # type: ignore[arg-type]

    enrichment = enricher.enrich(a_hit(title="My Article"))

    assert enrichment.published_at == datetime(2026, 3, 1, tzinfo=UTC)
    assert enrichment.event_type == EventType.FUNDING
    assert enrichment.summary == "$10M."
    assert llm.prompts[0] == ENRICHMENT_PROMPT.format(
        title="My Article", url="https://t", snippet="an excerpt"
    )


def test_claude_enricher_tolerates_structured_content_blocks() -> None:
    # Some models return content as a list of blocks; the adapter must not
    # crash — non-JSON stringification degrades to a neutral enrichment.
    blocks = [{"type": "text", "text": "{}"}]
    enricher = ClaudeHitEnricher("m", llm=FakeChatModel(content=blocks))  # type: ignore[arg-type]
    assert enricher.enrich(a_hit()) == HitEnrichment()


def test_claude_enricher_builds_the_real_client_when_a_key_is_present(monkeypatch) -> None:
    # Construction only — no request is ever sent (ADR-012).
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key-never-used")
    enricher = ClaudeHitEnricher("claude-opus-4-8")
    assert enricher._llm is not None  # noqa: SLF001 - asserting the wiring


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


def test_claude_policy_shows_the_transcript_and_parses_the_decision() -> None:
    llm = FakeChatModel(content='{"action": "search", "query": "rust news", "reason": "start"}')
    policy = ClaudeAgentPolicy("claude-opus-4-8", llm=llm)  # type: ignore[arg-type]
    steps = [AgentStep(seq=1, kind=AgentStepKind.SEARCH, detail="rust", reason="r", new_hits=2)]

    action = policy.decide("rust", steps, [a_hit(title="Rust 1.99 released")])

    assert action == SearchAction(query="rust news", reason="start")
    prompt = llm.prompts[0]
    assert "Goal: rust" in prompt
    assert '- "rust" -> 2 new' in prompt
    assert "- Rust 1.99 released" in prompt


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


def test_claude_critic_lists_the_results_and_parses_the_verdict() -> None:
    llm = FakeChatModel(
        content='{"assessment": "One gap.", "irrelevant_urls": [], "gap_query": "rust 2026"}'
    )
    critic = ClaudeResultCritic("claude-opus-4-8", llm=llm)  # type: ignore[arg-type]

    critique = critic.critique("rust", [a_hit(title="Rust 1.99 released")])

    assert critique == Critique(assessment="One gap.", gap_query="rust 2026")
    prompt = llm.prompts[0]
    assert "Goal: rust" in prompt and "- Rust 1.99 released" in prompt
