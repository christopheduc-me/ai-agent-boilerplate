"""Cross-language contract fixtures (ADR-025): the agent side.

The agent PRODUCES the callback bodies (they must serialize to exactly the
fixtures the Rust backend consumes) and CONSUMES the task request (the fixture
the Rust dispatcher produces must parse).

The producer tests drive the **real** ``HttpResultSink`` (over a mocked
transport) and assert the captured request body equals the shared fixture — the
same file the backend's ``contract.rs`` consumes. So the fixture is the single
source of truth for the wire shape: renaming a key, wrapping differently or
hitting the wrong endpoint breaks this suite instead of surfacing in the e2e
test (or production). ``test_adapters.py`` keeps the finer-grained sink unit
tests (headers, correlation id, error handling).
"""

import json
from datetime import UTC, datetime
from pathlib import Path

import httpx
import respx

from aiagent.adapters.api.app import TaskRequest
from aiagent.adapters.sink import HttpResultSink
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
    DateConfidence,
    EventType,
    ResearchResult,
)
from aiagent.domain.usage import Pricing, Usage

CONTRACTS = Path(__file__).parents[2] / "contracts"
BASE = "http://backend:8000"
JOB = "job-1"


def load(name: str) -> dict:
    return json.loads((CONTRACTS / name).read_text())


def _sink() -> HttpResultSink:
    return HttpResultSink(BASE, "secret-token")


def _captured_body(route: respx.Route) -> dict:
    return json.loads(route.calls.last.request.content)


# ---------------------------------------------------------------- producer side


@respx.mock
def test_agent_delivers_exactly_the_results_callback() -> None:
    route = respx.post(f"{BASE}/internal/jobs/{JOB}/results").mock(return_value=httpx.Response(204))
    results = [
        ResearchResult(
            title="provider-dated",
            url="https://example.com/provider-dated",
            snippet="Date supplied by the search provider",
            published_at=datetime(2026, 5, 1, tzinfo=UTC),
            date_confidence=DateConfidence.HIGH,
            event_type=EventType.RELEASE,
            summary="Version 2.0 was released with breaking changes.",
            raw={"provider": "fixture"},
        ),
        ResearchResult(
            title="llm-dated",
            url="https://example.com/llm-dated",
            snippet="Date extracted by the LLM",
            published_at=datetime(2025, 8, 20, 9, 30, tzinfo=UTC),
            date_confidence=DateConfidence.MEDIUM,
            event_type=EventType.FUNDING,
            summary="The company raised a Series A round.",
            raw={"provider": "fixture"},
        ),
        ResearchResult(
            title="undated",
            url="https://example.com/undated",
            snippet="No publication date could be determined",
            published_at=None,
            date_confidence=DateConfidence.UNKNOWN,
            event_type=EventType.OTHER,
            summary=None,
            raw={"provider": "fixture"},
        ),
    ]

    _sink().deliver(JOB, results)

    assert route.calls.last.request.headers["x-internal-token"] == "secret-token"
    assert _captured_body(route) == load("results-callback.json")


@respx.mock
def test_agent_reports_exactly_the_step_callback() -> None:
    route = respx.post(f"{BASE}/internal/jobs/{JOB}/steps").mock(return_value=httpx.Response(204))
    step = AgentStep(
        seq=1,
        kind=AgentStepKind.SEARCH,
        detail="rust hexagonal architecture",
        reason="Start with the user's goal as the query",
        new_hits=4,
    )

    _sink().report_step(JOB, step)

    assert _captured_body(route) == load("agent-step-callback.json")


@respx.mock
def test_agent_reports_exactly_the_usage_callback() -> None:
    route = respx.post(f"{BASE}/internal/jobs/{JOB}/usage").mock(return_value=httpx.Response(204))
    # ADR-038: 8500 in-tokens * $5/MTok + 1200 out * $25/MTok + 2 * $0.008.
    usage = Usage(llm_calls=9, llm_input_tokens=8500, llm_output_tokens=1200, search_calls=2)
    pricing = Pricing(llm_input_per_mtok=5.0, llm_output_per_mtok=25.0, search_per_call=0.008)

    _sink().report_usage(JOB, usage, pricing)

    assert _captured_body(route) == load("usage-callback.json")


@respx.mock
def test_agent_reports_exactly_the_failure_callback() -> None:
    # Drive the real sink so a renamed/added key breaks the contract — the
    # shape-only fixture check could not catch that (ADR-025 hardening).
    route = respx.post(f"{BASE}/internal/jobs/{JOB}/failure").mock(return_value=httpx.Response(204))
    fixture = load("failure-callback.json")

    _sink().report_failure(JOB, fixture["error"])

    assert _captured_body(route) == fixture


@respx.mock
def test_agent_requests_exactly_the_question_callback() -> None:
    # HITL (ADR-032): pauses the job with a question for the user.
    route = respx.post(f"{BASE}/internal/jobs/{JOB}/question").mock(
        return_value=httpx.Response(204)
    )
    fixture = load("question-callback.json")

    _sink().request_clarification(JOB, fixture["question"])

    assert _captured_body(route) == fixture


# ---------------------------------------------------------------- consumer side


def test_agent_consumes_the_task_request_produced_by_the_backend() -> None:
    request = TaskRequest(**load("task-request.json"))
    assert request.job_id == "3fa85f64-5717-4562-b3fc-2c963f66afa6"
    assert request.keyword == "rust hexagonal architecture"


def test_agent_consumes_the_task_request_mode() -> None:
    request = TaskRequest(**load("task-request.json"))
    assert request.mode == "agent"


def test_agent_consumes_the_task_request_clarification() -> None:
    request = TaskRequest(**load("task-request.json"))
    assert request.clarification is None  # first dispatch: no answer yet


def test_agent_consumes_the_task_request_recurring_memory() -> None:
    request = TaskRequest(**load("task-request.json"))
    # One-shot dispatch (ADR-033): not a recurring run, no memory.
    assert request.recurring is False
    assert request.seen_urls == []
