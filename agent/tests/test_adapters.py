import json
from datetime import UTC, datetime

import httpx
import respx

from aiagent.adapters.llm import EXTRACTION_PROMPT, ClaudeDateExtractor, parse_extracted_date
from aiagent.adapters.sink import HttpResultSink, serialize_result
from aiagent.adapters.tavily import hit_from_tavily
from aiagent.domain.models import DateConfidence, RawSearchHit, ResearchResult

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


# ---------------------------------------------------------------- ClaudeDateExtractor


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


def test_claude_extractor_builds_the_prompt_and_parses_the_reply() -> None:
    llm = FakeChatModel(content="2026-03-01")
    extractor = ClaudeDateExtractor("claude-opus-4-8", llm=llm)  # type: ignore[arg-type]

    extracted = extractor.extract_date(a_hit(title="My Article"))

    assert extracted == datetime(2026, 3, 1, tzinfo=UTC)
    prompt = llm.prompts[0]
    assert prompt == EXTRACTION_PROMPT.format(
        title="My Article", url="https://t", snippet="an excerpt"
    )


def test_claude_extractor_returns_none_on_unknown() -> None:
    extractor = ClaudeDateExtractor("m", llm=FakeChatModel(content="unknown"))  # type: ignore[arg-type]
    assert extractor.extract_date(a_hit()) is None


def test_claude_extractor_tolerates_structured_content_blocks() -> None:
    # Some models return content as a list of blocks; the adapter must not
    # crash, and a non-ISO stringification means "unknown".
    blocks = [{"type": "text", "text": "2026-03-01"}]
    extractor = ClaudeDateExtractor("m", llm=FakeChatModel(content=blocks))  # type: ignore[arg-type]
    assert extractor.extract_date(a_hit()) is None


def test_claude_extractor_builds_the_real_client_when_a_key_is_present(monkeypatch) -> None:
    # Construction only — no request is ever sent (ADR-012).
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key-never-used")
    extractor = ClaudeDateExtractor("claude-opus-4-8")
    assert extractor._llm is not None  # noqa: SLF001 - asserting the wiring


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
