from datetime import UTC, datetime

import pytest

from aiagent.application import run_research
from aiagent.domain.models import DateConfidence, RawSearchHit, ResearchResult


class FakeSearch:
    def __init__(self, hits: list[RawSearchHit] | None = None, error: Exception | None = None):
        self.hits = hits or []
        self.error = error

    def search(self, keyword: str) -> list[RawSearchHit]:
        if self.error:
            raise self.error
        return self.hits


class FakeDateExtractor:
    """Returns a fixed date for hits whose title is in `known`."""

    def __init__(self, known: dict[str, datetime] | None = None):
        self.known = known or {}
        self.seen: list[str] = []

    def extract_date(self, hit: RawSearchHit) -> datetime | None:
        self.seen.append(hit.title)
        return self.known.get(hit.title)


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
    extractor = FakeDateExtractor(known={"from-llm": datetime(2025, 5, 5, tzinfo=UTC)})
    sink = RecordingSink()

    results = run_research("job-1", "keyword", search, extractor, sink)

    by_title = {r.title: r for r in results}
    assert by_title["from-provider"].date_confidence == DateConfidence.HIGH
    assert by_title["from-llm"].date_confidence == DateConfidence.MEDIUM
    assert by_title["undatable"].date_confidence == DateConfidence.UNKNOWN
    # The LLM is only consulted when the provider gave no date (cost control).
    assert extractor.seen == ["from-llm", "undatable"]


def test_delivers_results_sorted_newest_first() -> None:
    search = FakeSearch(
        hits=[
            hit("old", published_at=datetime(2023, 1, 1, tzinfo=UTC)),
            hit("new", published_at=datetime(2026, 1, 1, tzinfo=UTC)),
            hit("undatable"),
        ]
    )
    sink = RecordingSink()

    run_research("job-1", "keyword", search, FakeDateExtractor(), sink)

    (job_id, delivered) = sink.delivered[0]
    assert job_id == "job-1"
    assert [r.title for r in delivered] == ["new", "old", "undatable"]


def test_marks_the_job_started_before_searching() -> None:
    sink = RecordingSink()

    run_research("job-1", "keyword", FakeSearch(), FakeDateExtractor(), sink)

    assert sink.started == ["job-1"]


def test_reports_failure_and_reraises_when_search_breaks() -> None:
    search = FakeSearch(error=RuntimeError("quota exceeded"))
    sink = RecordingSink()

    with pytest.raises(RuntimeError):
        run_research("job-1", "keyword", search, FakeDateExtractor(), sink)

    assert sink.failures == [("job-1", "quota exceeded")]
    assert sink.delivered == []


def test_original_error_survives_when_failure_report_also_breaks() -> None:
    """Backend unreachable: the root cause must reach Celery, not the sink error."""
    search = FakeSearch(error=RuntimeError("quota exceeded"))
    sink = RecordingSink(failure_error=ConnectionError("backend down"))

    with pytest.raises(RuntimeError, match="quota exceeded"):
        run_research("job-1", "keyword", search, FakeDateExtractor(), sink)
