"""ResultSink + StepReporter adapter: HTTP callbacks to the Rust backend (ADR-006/030)."""

from typing import Any

import httpx

from aiagent.domain.models import AgentStep, ResearchResult
from aiagent.domain.usage import Pricing, Usage


def serialize_result(result: ResearchResult) -> dict[str, Any]:
    return {
        "title": result.title,
        "url": result.url,
        "snippet": result.snippet,
        "published_at": result.published_at.isoformat() if result.published_at else None,
        "date_confidence": result.date_confidence.value,
        "event_type": result.event_type.value,
        "summary": result.summary,
        "is_new": result.is_new,
        "raw": result.raw,
    }


def serialize_usage(usage: Usage, pricing: Pricing) -> dict[str, Any]:
    return {
        "llm_calls": usage.llm_calls,
        "llm_input_tokens": usage.llm_input_tokens,
        "llm_output_tokens": usage.llm_output_tokens,
        "search_calls": usage.search_calls,
        "cost_usd": usage.cost_usd(pricing),
    }


def serialize_step(step: AgentStep) -> dict[str, Any]:
    return {
        "seq": step.seq,
        "kind": step.kind.value,
        "detail": step.detail,
        "reason": step.reason,
        "new_hits": step.new_hits,
    }


class HttpResultSink:
    def __init__(
        self,
        base_url: str,
        internal_token: str,
        client: httpx.Client | None = None,
        request_id: str | None = None,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._client = client or httpx.Client(timeout=30)
        self._headers = {"X-Internal-Token": internal_token}
        if request_id:
            # Correlation (ADR-018): callbacks carry the id end to end.
            self._headers["X-Request-Id"] = request_id

    def mark_started(self, job_id: str) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/started",
            headers=self._headers,
        )
        response.raise_for_status()

    def deliver(self, job_id: str, results: list[ResearchResult]) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/results",
            json={"results": [serialize_result(r) for r in results]},
            headers=self._headers,
        )
        response.raise_for_status()

    def request_clarification(self, job_id: str, question: str) -> None:
        # HITL (ADR-032): pauses the job with a question for the user.
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/question",
            json={"question": question},
            headers=self._headers,
        )
        response.raise_for_status()

    def report_usage(self, job_id: str, usage: Usage, pricing: Pricing) -> None:
        # Spend tracking (ADR-038); the task treats failures as best-effort.
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/usage",
            json=serialize_usage(usage, pricing),
            headers=self._headers,
        )
        response.raise_for_status()

    def report_step(self, job_id: str, step: AgentStep) -> None:
        # Live journal (ADR-030); the use case treats failures as best-effort.
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/steps",
            json=serialize_step(step),
            headers=self._headers,
        )
        response.raise_for_status()

    def report_failure(self, job_id: str, error: str) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/failure",
            json={"error": error},
            headers=self._headers,
        )
        response.raise_for_status()
