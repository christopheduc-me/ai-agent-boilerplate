"""Live Ollama test (ADR-041): opt-in — free, but needs a local server.

Run explicitly, with an Ollama server up and the model pulled:

    ollama pull qwen3:14b
    RUN_OLLAMA_TESTS=1 AGENT_LLM_BACKEND=ollama AGENT_MODEL_ID=qwen3:14b \
        uv run pytest tests/test_live_ollama.py -v

Never run in CI (needs a GPU-ish host and a pulled model). Purpose: the same
drift check as the paid live tests (ADR-012), for the local backend — a small
model that stops following the JSON instructions degrades silently through
the defensive parsing (dates become `unknown`, the policy finishes early).
"""

import os
from datetime import UTC, datetime

import pytest

from aiagent.domain.models import EventType, RawSearchHit

pytestmark = pytest.mark.skipif(
    os.environ.get("RUN_OLLAMA_TESTS") != "1",
    reason="Ollama live test is opt-in (RUN_OLLAMA_TESTS=1) — it needs a local server",
)


def test_local_model_extracts_the_stated_date_and_type() -> None:
    from aiagent.adapters.chat_model import make_chat_model
    from aiagent.adapters.llm import LlmHitEnricher
    from aiagent.config import Settings

    settings = Settings.from_env()
    assert settings.llm_backend == "ollama", "run with AGENT_LLM_BACKEND=ollama"

    hit = RawSearchHit(
        title="Rust 1.99 released with faster incremental builds",
        url="https://blog.rust-lang.org/2026/03/12/Rust-1.99.0.html",
        snippet=(
            "The Rust team published this release announcement on 12 March 2026. "
            "Rust 1.99 ships faster incremental builds and stabilizes several APIs."
        ),
    )
    enrichment = LlmHitEnricher(make_chat_model(settings, max_tokens=256)).enrich(hit)

    # The bar is the same as for the hosted model: an explicitly stated date
    # must come back as ISO, and the event must be recognizably a release.
    assert enrichment.published_at is not None, "local model failed to extract an explicit date"
    assert enrichment.published_at.date() == datetime(2026, 3, 12, tzinfo=UTC).date()
    assert enrichment.event_type in (EventType.RELEASE, EventType.ANNOUNCEMENT)
    assert enrichment.summary, "local model returned no summary"
