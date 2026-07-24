"""The research use case: pure orchestration over the ports, no framework, no I/O."""

from aiagent.domain.models import (
    DateConfidence,
    HitEnrichment,
    RawSearchHit,
    ResearchResult,
    as_utc,
    dedupe_hits,
    flag_new,
    sort_by_publication_date,
)
from aiagent.domain.ports import HitEnricher, PageDateFetcher, ResultSink, SearchProvider


def resolve_hits(
    hits: list[RawSearchHit],
    enricher: HitEnricher,
    page_dates: PageDateFetcher | None = None,
) -> list[ResearchResult]:
    """Enriches a whole result set through one batched port call (ADR-042 —
    the adapter parallelizes the per-hit LLM calls), then resolves each date."""
    enrichments = enricher.enrich_many(hits)
    return [
        resolve_hit(hit, enrichment, page_dates)
        for hit, enrichment in zip(hits, enrichments, strict=True)
    ]


def resolve_hit(
    hit: RawSearchHit,
    enrichment: HitEnrichment,
    page_dates: PageDateFetcher | None = None,
) -> ResearchResult:
    """Date cascade (ADR-011/035) over an already-computed enrichment: the
    provider's date wins (high); else the date the page declares about itself
    (high, ADR-035); else the LLM's guess (medium); else unknown. The page is
    only fetched when the provider gave no date."""
    page_date = None
    if hit.published_at is None and page_dates is not None:
        page_date = page_dates.fetch_published_date(hit.url)
    if hit.published_at is not None:
        published_at, confidence = as_utc(hit.published_at), DateConfidence.HIGH
    elif page_date is not None:
        published_at, confidence = as_utc(page_date), DateConfidence.HIGH
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
    page_dates: PageDateFetcher | None = None,
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
        results = sort_by_publication_date(resolve_hits(hits, enricher, page_dates))
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
