"""Ports (hexagonal architecture): the use case depends only on these Protocols."""

from typing import Protocol

from aiagent.domain.models import (
    AgentAction,
    AgentStep,
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


class StepReporter(Protocol):
    """Publishes each executed decision for the live journal (ADR-030).
    Best-effort by contract: a failed report never fails the job."""

    def report_step(self, job_id: str, step: AgentStep) -> None: ...


class HitEnricher(Protocol):
    """LLM-backed enrichment (ADR-027): one call per hit returning the
    publication date (used only when the provider gave none, ADR-011), the
    event type, and a one-line summary for the timeline."""

    def enrich(self, hit: RawSearchHit) -> HitEnrichment: ...


class ResultSink(Protocol):
    """Job lifecycle callbacks — in production, the Rust API (ADR-006/016)."""

    def mark_started(self, job_id: str) -> None: ...

    def deliver(self, job_id: str, results: list[ResearchResult]) -> None: ...

    def report_failure(self, job_id: str, error: str) -> None: ...
