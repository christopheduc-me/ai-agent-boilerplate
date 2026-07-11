"""Ports (hexagonal architecture): the use case depends only on these Protocols."""

from typing import Protocol

from aiagent.domain.models import HitEnrichment, RawSearchHit, ResearchResult


class SearchProvider(Protocol):
    def search(self, keyword: str) -> list[RawSearchHit]: ...


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
