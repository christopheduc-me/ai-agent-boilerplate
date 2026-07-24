"""Ports (hexagonal architecture): the use case depends only on these Protocols."""

from datetime import datetime
from typing import Protocol

from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    Critique,
    HitEnrichment,
    RawSearchHit,
    ResearchResult,
)


class SearchProvider(Protocol):
    def search(self, keyword: str) -> list[RawSearchHit]: ...


class AgentPolicy(Protocol):
    """The decision-maker of the agentic loop (ADR-030): given the goal and
    everything done/found so far, picks the next action. In production an LLM;
    in tests a scripted fake — the loop itself stays deterministic."""

    def decide(
        self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]
    ) -> AgentAction: ...


class ClarificationRequester(Protocol):
    """Pauses the job with a question for the user (ADR-032) — in production
    the `POST /internal/jobs/{id}/question` callback."""

    def request_clarification(self, job_id: str, question: str) -> None: ...


class ResultCritic(Protocol):
    """Self-critique before delivery (ADR-031): judges the collected hits
    against the goal. In production an LLM; a malformed reply must degrade to
    a neutral critique inside the adapter, never fail the job."""

    def critique(self, goal: str, hits: list[RawSearchHit]) -> Critique: ...


class StepReporter(Protocol):
    """Publishes each executed decision for the live journal (ADR-030).
    Best-effort by contract: a failed report never fails the job."""

    def report_step(self, job_id: str, step: AgentStep) -> None: ...


class PageDateFetcher(Protocol):
    """Stage 2 of the date cascade (ADR-035): reads the publication date the
    page itself declares (JSON-LD / OpenGraph). Source-authoritative, so its
    dates rank `high` — above the LLM's guess. Must never raise: an
    unreachable or unparseable page simply returns None."""

    def fetch_published_date(self, url: str) -> datetime | None: ...


class HitEnricher(Protocol):
    """LLM-backed enrichment (ADR-027): one LLM call per hit returning the
    publication date (used only when the provider gave none, ADR-011), the
    event type, and a one-line summary for the timeline. The port is
    batch-shaped (ADR-042): both use cases enrich a whole result set at once,
    so adapters can issue the per-hit calls concurrently. Returns one
    enrichment per hit, in the same order."""

    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]: ...


class ResultSink(Protocol):
    """Job lifecycle callbacks — in production, the Rust API (ADR-006/016)."""

    def mark_started(self, job_id: str) -> None: ...

    def deliver(self, job_id: str, results: list[ResearchResult]) -> None: ...

    def report_failure(self, job_id: str, error: str) -> None: ...
