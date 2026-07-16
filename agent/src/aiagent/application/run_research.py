"""The research use case: pure orchestration over the ports, no framework, no I/O."""

from aiagent.domain.models import (
    DateConfidence,
    RawSearchHit,
    ResearchResult,
    as_utc,
    dedupe_hits,
    flag_new,
    sort_by_publication_date,
)
from aiagent.domain.ports import HitEnricher, ResultSink, SearchProvider


def resolve_hit(hit: RawSearchHit, enricher: HitEnricher) -> ResearchResult:
    """Enrichment (ADR-027) + date cascade (ADR-011): the provider's date wins
    (high confidence); otherwise the LLM's date is used (medium); else unknown."""
    enrichment = enricher.enrich(hit)
    if hit.published_at is not None:
        published_at, confidence = as_utc(hit.published_at), DateConfidence.HIGH
    elif enrichment.published_at is not None:
        published_at, confidence = as_utc(enrichment.published_at), DateConfidence.MEDIUM
    else:
        published_at, confidence = None, DateConfidence.UNKNOWN
    return ResearchResult(
        title=hit.title,
        url=hit.url,
        snippet=hit.snippet,
        published_at=published_at,
        date_confidence=confidence,
        event_type=enrichment.event_type,
        summary=enrichment.summary,
        raw=hit.raw,
    )


def run_research(
    job_id: str,
    keyword: str,
    search: SearchProvider,
    enricher: HitEnricher,
    sink: ResultSink,
    seen_urls: set[str] | None = None,
) -> list[ResearchResult]:
    """Marks the job running, searches, enriches every hit, sorts, delivers.

    On failure the sink is notified (best effort) and the exception propagates
    so Celery retries the task; the whole flow is idempotent (`mark_started` is
    a no-op on a non-pending job, delivery replaces previous results).
    """
    try:
        sink.mark_started(job_id)
        # Canonical-URL deduplication (ADR-034): drop retagged duplicates
        # before paying for their enrichment.
        hits = dedupe_hits(search.search(keyword))
        results = sort_by_publication_date([resolve_hit(hit, enricher) for hit in hits])
        if seen_urls is not None:
            # Recurring run (ADR-033): flag what previous runs already saw.
            results = flag_new(results, seen_urls)
        sink.deliver(job_id, results)
        return results
    except Exception as exc:
        try:
            sink.report_failure(job_id, str(exc))
        except Exception:  # noqa: BLE001 - keep the original error as the cause
            pass
        raise
