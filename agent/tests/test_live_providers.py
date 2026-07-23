"""Live provider tests (ADR-012): opt-in — they call the PAID services.

Run explicitly, with real keys in the environment (repo-root `.env`):

    RUN_LIVE_TESTS=1 uv run pytest tests/test_live_providers.py -v

Never run in CI (cost, keys, network flakiness). Purpose: catch **provider
drift** — renamed fields, changed reply shapes, a model that stops following
the JSON instructions — which the defensive parsing everywhere else would
otherwise degrade silently (dates quietly becoming `unknown`, event types
quietly becoming `other`). Run them after bumping `AGENT_MODEL_ID` or when a
deployment reports degraded extraction quality.
"""

import os
from datetime import UTC, datetime

import pytest

from aiagent.domain.models import EventType, RawSearchHit, SearchAction

pytestmark = pytest.mark.skipif(
    os.environ.get("RUN_LIVE_TESTS") != "1",
    reason="live provider tests are opt-in (RUN_LIVE_TESTS=1) — they cost API credits",
)


def release_hit() -> RawSearchHit:
    """A hit whose snippet states the publication date and nature explicitly —
    a healthy model must extract both."""
    return RawSearchHit(
        title="Rust 1.99 released with faster incremental builds",
        url="https://blog.rust-lang.org/2026/03/12/Rust-1.99.0.html",
        snippet=(
            "The Rust team published this release announcement on 12 March 2026. "
            "Rust 1.99 ships faster incremental builds and stabilizes several APIs."
        ),
    )


def test_tavily_returns_hits_our_mapping_understands() -> None:
    from aiagent.adapters.tavily import TavilySearchProvider

    hits = TavilySearchProvider(max_results=5).search("rust programming language news")

    assert hits, "live Tavily search returned no results"
    for hit in hits:
        assert hit.url.startswith("http")
        assert hit.title
    # Field-name drift check: the raw items still carry the keys we map
    # (a rename would silently empty titles/snippets otherwise).
    assert all({"title", "url", "content"} <= set(h.raw) for h in hits)


def test_claude_enricher_extracts_the_stated_date_and_type() -> None:
    from aiagent.adapters.chat_model import make_chat_model
    from aiagent.adapters.llm import LlmHitEnricher
    from aiagent.config import Settings

    enrichment = LlmHitEnricher(make_chat_model(Settings.from_env(), max_tokens=256)).enrich(
        release_hit()
    )

    # The snippet states the date in prose: the model must return it as ISO
    # (a drift here means dates silently fall back to `medium`/`unknown`).
    assert enrichment.published_at is not None, "model failed to extract an explicit date"
    assert enrichment.published_at.date() == datetime(2026, 3, 12, tzinfo=UTC).date()
    # A release announcement: either label is defensible, anything else is drift.
    assert enrichment.event_type in (EventType.RELEASE, EventType.ANNOUNCEMENT)
    assert enrichment.summary, "model returned no summary"


def test_claude_policy_starts_an_unambiguous_goal_with_a_search() -> None:
    from aiagent.adapters.chat_model import make_chat_model
    from aiagent.adapters.llm import LlmAgentPolicy
    from aiagent.config import Settings

    action = LlmAgentPolicy(make_chat_model(Settings.from_env(), max_tokens=256)).decide(
        "rust 1.99 release notes and reactions", [], []
    )

    # Fresh loop, clear goal, nothing collected: a sane policy searches.
    assert isinstance(action, SearchAction), f"expected a search, got {action!r}"
    assert action.query.strip()
    assert action.reason.strip()


def test_claude_critic_keeps_on_topic_results() -> None:
    from aiagent.adapters.chat_model import make_chat_model
    from aiagent.adapters.llm import LlmResultCritic
    from aiagent.config import Settings

    hits = [
        release_hit(),
        RawSearchHit(
            title="Rust 1.99 review: what the new release means in practice",
            url="https://example-tech-blog.com/rust-1-99-review",
            snippet="A hands-on look at the Rust 1.99 release and its build-time gains.",
        ),
    ]
    critique = LlmResultCritic(make_chat_model(Settings.from_env(), max_tokens=512)).critique(
        "rust 1.99 release", hits
    )

    assert critique.assessment.strip(), "model returned no assessment"
    # The prompt asks for conservative drops: both hits are clearly on topic.
    assert len(critique.irrelevant_urls) == 0, f"dropped on-topic: {critique.irrelevant_urls}"
