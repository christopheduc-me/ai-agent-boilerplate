"""Multi-provider search aggregation (ADR-051): pure Reciprocal Rank Fusion +
the concurrent, partial-failure-tolerant AggregatingSearchProvider. Driven with
fake providers — no network."""

import time
from datetime import UTC, datetime

import pytest

from aiagent.adapters.aggregating_search import AggregatingSearchProvider, fuse_by_rrf
from aiagent.domain.models import RawSearchHit


def hit(url: str, *, dated: bool = False) -> RawSearchHit:
    return RawSearchHit(
        title="t",
        url=url,
        snippet="s",
        published_at=datetime(2026, 1, 1, tzinfo=UTC) if dated else None,
    )


class FakeProvider:
    def __init__(
        self,
        hits: list[RawSearchHit] | None = None,
        error: Exception | None = None,
        delay: float = 0.0,
    ) -> None:
        self._hits = hits or []
        self._error = error
        self._delay = delay
        self.calls: list[str] = []

    def search(self, keyword: str) -> list[RawSearchHit]:
        self.calls.append(keyword)
        if self._delay:
            time.sleep(self._delay)
        if self._error:
            raise self._error
        return list(self._hits)


# ---------------------------------------------------------------- fusion (RRF)


def test_rrf_dedups_by_canonical_url_and_rewards_agreement() -> None:
    a = [hit("https://x"), hit("https://y")]  # y at rank 2
    b = [hit("https://y"), hit("https://z")]  # y at rank 1
    fused = fuse_by_rrf([a, b])
    # y is found by both -> highest score; deduped to 3 unique results.
    assert [h.url for h in fused] == ["https://y", "https://x", "https://z"]


def test_rrf_matches_retagged_urls_and_keeps_the_richer_hit() -> None:
    # Same page, tracking-tagged in one provider (ADR-034), undated there but
    # dated in the other — the merged hit keeps the date.
    a = [hit("https://ex.com/post?utm_source=a")]
    b = [hit("https://ex.com/post", dated=True)]
    fused = fuse_by_rrf([a, b])
    assert len(fused) == 1
    assert fused[0].published_at is not None


# ---------------------------------------------------------------- aggregator


def test_aggregates_results_from_all_providers() -> None:
    p1 = FakeProvider([hit("https://a"), hit("https://b")])
    p2 = FakeProvider([hit("https://b"), hit("https://c")])  # b overlaps
    agg = AggregatingSearchProvider([p1, p2])
    urls = {h.url for h in agg.search("q")}
    assert urls == {"https://a", "https://b", "https://c"}
    assert p1.calls == ["q"] and p2.calls == ["q"]


def test_tolerates_a_failing_provider() -> None:
    good = FakeProvider([hit("https://a")])
    bad = FakeProvider(error=RuntimeError("provider down"))
    agg = AggregatingSearchProvider([good, bad])
    # One provider failed; the run continues with the other's results.
    assert [h.url for h in agg.search("q")] == ["https://a"]


def test_treats_a_timeout_as_a_failed_provider() -> None:
    fast = FakeProvider([hit("https://a")])
    slow = FakeProvider([hit("https://slow")], delay=0.3)
    agg = AggregatingSearchProvider([fast, slow], timeout_s=0.05)
    assert [h.url for h in agg.search("q")] == ["https://a"]


def test_raises_only_when_every_provider_fails() -> None:
    p1 = FakeProvider(error=RuntimeError("down 1"))
    p2 = FakeProvider(error=RuntimeError("down 2"))
    agg = AggregatingSearchProvider([p1, p2])
    with pytest.raises(RuntimeError, match="all 2 search providers failed"):
        agg.search("q")
