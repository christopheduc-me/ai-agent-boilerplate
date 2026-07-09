"""The research use case: pure orchestration over the ports, no framework, no I/O."""

from aiagent.domain.models import (
    DateConfidence,
    RawSearchHit,
    ResearchResult,
    as_utc,
    sort_by_publication_date,
)
from aiagent.domain.ports import DateExtractor, ResultSink, SearchProvider


def _resolve(hit: RawSearchHit, date_extractor: DateExtractor) -> ResearchResult:
    """Date cascade (ADR-011): provider metadata -> LLM extraction -> unknown."""
    if hit.published_at is not None:
        published_at, confidence = as_utc(hit.published_at), DateConfidence.HIGH
    elif (extracted := date_extractor.extract_date(hit)) is not None:
        published_at, confidence = as_utc(extracted), DateConfidence.MEDIUM
    else:
        published_at, confidence = None, DateConfidence.UNKNOWN
    return ResearchResult(
        title=hit.title,
        url=hit.url,
        snippet=hit.snippet,
        published_at=published_at,
        date_confidence=confidence,
        raw=hit.raw,
    )


def run_research(
    job_id: str,
    keyword: str,
    search: SearchProvider,
    date_extractor: DateExtractor,
    sink: ResultSink,
) -> list[ResearchResult]:
    """Marks the job running, searches, resolves dates, sorts, and delivers.

    On failure the sink is notified (best effort) and the exception propagates
    so Celery retries the task; the whole flow is idempotent (`mark_started` is
    a no-op on a non-pending job, delivery replaces previous results).
    """
    try:
        sink.mark_started(job_id)
        hits = search.search(keyword)
        results = sort_by_publication_date([_resolve(hit, date_extractor) for hit in hits])
        sink.deliver(job_id, results)
        return results
    except Exception as exc:
        try:
            sink.report_failure(job_id, str(exc))
        except Exception:  # noqa: BLE001 - keep the original error as the cause
            pass
        raise
