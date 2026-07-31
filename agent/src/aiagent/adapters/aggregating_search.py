"""Multi-provider search aggregation (ADR-051).

An `AggregatingSearchProvider` is a `SearchProvider` (the port, ADR-009) that
wraps several inner providers — Tavily, DuckDuckGo, … — queries them
concurrently, and fuses their results into one ranked, deduplicated list. It is
a pure adapter-layer composition: the agent loop, the domain and the ports are
untouched (the hexagonal payoff).

Two design choices:

- **Reciprocal Rank Fusion** (`fuse_by_rrf`): a result ranked well by several
  engines outranks one found by a single engine, with no weights to tune.
  Deduplication is by canonical URL (ADR-034); the richer hit (one carrying a
  date) wins a tie.
- **Partial-failure tolerance**: unlike a single provider (which fails the job
  on error, ADR-009), the aggregator logs a failing/slow provider and continues
  with the others — it only raises when *every* provider fails. Each provider is
  bounded by `timeout_s` so a slow engine cannot stall the run.

Usage metering (ADR-038) is unchanged: each inner provider records its own
search credit, so N providers = N credits per aggregated query.
"""

import logging
from concurrent.futures import ThreadPoolExecutor, wait

from aiagent.domain.models import RawSearchHit
from aiagent.domain.ports import SearchProvider
from aiagent.domain.urls import normalize_url

logger = logging.getLogger(__name__)


def fuse_by_rrf(
    results_per_provider: list[list[RawSearchHit]], *, k: int = 60
) -> list[RawSearchHit]:
    """Reciprocal Rank Fusion over each provider's ranked results, deduplicated
    by canonical URL (ADR-034). A URL's score is the sum over providers of
    ``1 / (k + rank)`` (rank 1-based); `k=60` is the standard default. On a
    duplicate, the richer hit (one with a publication date) is kept."""
    scores: dict[str, float] = {}
    best: dict[str, RawSearchHit] = {}
    for hits in results_per_provider:
        for rank, hit in enumerate(hits, start=1):
            key = normalize_url(hit.url)
            scores[key] = scores.get(key, 0.0) + 1.0 / (k + rank)
            incumbent = best.get(key)
            if incumbent is None or (hit.published_at and not incumbent.published_at):
                best[key] = hit
    return sorted(best.values(), key=lambda h: scores[normalize_url(h.url)], reverse=True)


class AggregatingSearchProvider:
    """A `SearchProvider` fanning one query out to several inner providers,
    concurrently and fault-tolerantly, then fusing the results."""

    def __init__(self, providers: list[SearchProvider], *, timeout_s: float = 10.0) -> None:
        if not providers:
            raise ValueError("AggregatingSearchProvider needs at least one provider")
        self._providers = list(providers)
        self._timeout_s = timeout_s

    def search(self, keyword: str) -> list[RawSearchHit]:
        results: list[list[RawSearchHit]] = []
        with ThreadPoolExecutor(max_workers=len(self._providers)) as executor:
            futures = {executor.submit(p.search, keyword): p for p in self._providers}
            done, not_done = wait(futures, timeout=self._timeout_s)
            for future in not_done:
                future.cancel()
                logger.warning(
                    "search provider timed out", extra={"provider": type(futures[future]).__name__}
                )
            for future in done:
                try:
                    results.append(future.result())
                except Exception:  # noqa: BLE001 - one provider's failure is tolerated
                    logger.warning(
                        "search provider failed",
                        extra={"provider": type(futures[future]).__name__},
                        exc_info=True,
                    )
        # Fail only when nothing came back at all — otherwise a partial outage
        # (a rate-limited or slow engine) degrades gracefully to the survivors.
        if not results:
            raise RuntimeError(f"all {len(self._providers)} search providers failed")
        return fuse_by_rrf(results)
