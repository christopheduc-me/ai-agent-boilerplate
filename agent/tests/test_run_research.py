from datetime import UTC, datetime

import pytest

from aiagent.application import run_research
from aiagent.domain.models import (
    DateConfidence,
    EventType,
    HitEnrichment,
    RawSearchHit,
    ResearchResult,
)


class FakeSearch:
    def __init__(self, hits: list[RawSearchHit] | None = None, error: Exception | None = None):
        self.hits = hits or []
        self.error = error

    def search(self, keyword: str) -> list[RawSearchHit]:
        if self.error:
            raise self.error
        return self.hits


class FakeEnricher:
    """Returns a fixed date for hits whose title is in `known` (ADR-027)."""

    def __init__(self, known: dict[str, datetime] | None = None):
        self.known = known or {}
        self.seen: list[str] = []
        self.calls = 0

    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]:
        self.calls += 1
        self.seen.extend(hit.title for hit in hits)
        return [
            HitEnrichment(
                published_at=self.known.get(hit.title),
                event_type=EventType.RESEARCH,
                summary=f"summary of {hit.title}",
            )
            for hit in hits
        ]


class RecordingSink:
    def __init__(self, failure_error: Exception | None = None) -> None:
        self.started: list[str] = []
        self.delivered: list[tuple[str, list[ResearchResult]]] = []
        self.failures: list[tuple[str, str]] = []
        self.failure_error = failure_error

    def mark_started(self, job_id: str) -> None:
        self.started.append(job_id)

    def deliver(self, job_id: str, results: list[ResearchResult]) -> None:
        self.delivered.append((job_id, results))

    def report_failure(self, job_id: str, error: str) -> None:
        self.failures.append((job_id, error))
        if self.failure_error:
            raise self.failure_error


def hit(title: str, published_at: datetime | None = None) -> RawSearchHit:
    return RawSearchHit(
        title=title, url=f"https://x/{title}", snippet="s", published_at=published_at
    )


def test_date_cascade_provider_then_llm_then_unknown() -> None:
    search = FakeSearch(
        hits=[
            hit("from-provider", published_at=datetime(2026, 1, 1, tzinfo=UTC)),
            hit("from-llm"),
            hit("undatable"),
        ]
    )
    enricher = FakeEnricher(known={"from-llm": datetime(2025, 5, 5, tzinfo=UTC)})
    sink = RecordingSink()

    results = run_research("job-1", "keyword", search, enricher, sink)

    by_title = {r.title: r for r in results}
    assert by_title["from-provider"].date_confidence == DateConfidence.HIGH
    assert by_title["from-llm"].date_confidence == DateConfidence.MEDIUM
    assert by_title["undatable"].date_confidence == DateConfidence.UNKNOWN
    # Every hit is enriched (event type + summary, ADR-027) — even dated ones.
    assert enricher.seen == ["from-provider", "from-llm", "undatable"]
    # ADR-042: the whole result set is enriched in a single batched port call.
    assert enricher.calls == 1
    assert by_title["from-provider"].event_type == EventType.RESEARCH
    assert by_title["undatable"].summary == "summary of undatable"
    # The provider's date always wins over the LLM's (ADR-011).
    assert by_title["from-provider"].published_at == datetime(2026, 1, 1, tzinfo=UTC)


def test_delivers_results_sorted_newest_first() -> None:
    search = FakeSearch(
        hits=[
            hit("old", published_at=datetime(2023, 1, 1, tzinfo=UTC)),
            hit("new", published_at=datetime(2026, 1, 1, tzinfo=UTC)),
            hit("undatable"),
        ]
    )
    sink = RecordingSink()

    run_research("job-1", "keyword", search, FakeEnricher(), sink)

    (job_id, delivered) = sink.delivered[0]
    assert job_id == "job-1"
    assert [r.title for r in delivered] == ["new", "old", "undatable"]


def test_workflow_recurring_run_flags_seen_urls() -> None:
    # ADR-033: the fixed pipeline also honors the memory of previous runs.
    search = FakeSearch(hits=[hit("a"), hit("b")])
    sink = RecordingSink()

    results = run_research("job-r", "kw", search, FakeEnricher(), sink, seen_urls={"https://x/a"})

    assert {r.url: r.is_new for r in results} == {"https://x/a": False, "https://x/b": True}


class RecordingPageDates:
    """Returns a fixed date and records which URLs were actually fetched."""

    def __init__(self, date: datetime | None) -> None:
        self.date = date
        self.fetched: list[str] = []

    def fetch_published_date(self, url: str) -> datetime | None:
        self.fetched.append(url)
        return self.date


def test_page_declared_date_ranks_high_and_beats_the_llm() -> None:
    # ADR-035: the page's own metadata wins over the LLM guess, at high
    # confidence — and provider-dated hits are never fetched (cost guard).
    hits = [
        hit("provider-dated", published_at=datetime(2026, 1, 1, tzinfo=UTC)),
        hit("page-dated"),
    ]
    page_dates = RecordingPageDates(datetime(2025, 6, 1, tzinfo=UTC))
    enricher = FakeEnricher(known={"page-dated": datetime(2020, 1, 1, tzinfo=UTC)})
    sink = RecordingSink()

    results = run_research("job-p", "kw", FakeSearch(hits), enricher, sink, page_dates=page_dates)

    by_title = {r.title: r for r in results}
    assert by_title["page-dated"].date_confidence == DateConfidence.HIGH
    assert by_title["page-dated"].published_at == datetime(2025, 6, 1, tzinfo=UTC)
    # Only the undated hit was fetched.
    assert page_dates.fetched == ["https://x/page-dated"]


def test_workflow_deduplicates_retagged_urls() -> None:
    # ADR-034: the same article under different tracking params counts once.
    hits = [
        RawSearchHit(title="a", url="https://ex.com/p?utm_source=rss", snippet="s"),
        RawSearchHit(title="a-dup", url="https://EX.com/p/", snippet="s"),
        RawSearchHit(title="b", url="https://ex.com/q", snippet="s"),
    ]
    sink = RecordingSink()

    results = run_research("job-d", "kw", FakeSearch(hits), FakeEnricher(), sink)

    assert [r.title for r in results] == ["a", "b"]


def test_marks_the_job_started_before_searching() -> None:
    sink = RecordingSink()

    run_research("job-1", "keyword", FakeSearch(), FakeEnricher(), sink)

    assert sink.started == ["job-1"]


def test_reports_failure_and_reraises_when_search_breaks() -> None:
    search = FakeSearch(error=RuntimeError("quota exceeded"))
    sink = RecordingSink()

    with pytest.raises(RuntimeError):
        run_research("job-1", "keyword", search, FakeEnricher(), sink)

    assert sink.failures == [("job-1", "quota exceeded")]
    assert sink.delivered == []


def test_original_error_survives_when_failure_report_also_breaks() -> None:
    """Backend unreachable: the root cause must reach Celery, not the sink error."""
    search = FakeSearch(error=RuntimeError("quota exceeded"))
    sink = RecordingSink(failure_error=ConnectionError("backend down"))

    with pytest.raises(RuntimeError, match="quota exceeded"):
        run_research("job-1", "keyword", search, FakeEnricher(), sink)
