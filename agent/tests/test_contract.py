"""Cross-language contract fixtures (ADR-025): the agent side.

The agent PRODUCES the callback bodies (they must serialize to exactly the
fixtures the Rust backend consumes) and CONSUMES the task request (the fixture
the Rust dispatcher produces must parse).
"""

import json
from datetime import UTC, datetime
from pathlib import Path

from aiagent.adapters.api.app import TaskRequest
from aiagent.adapters.sink import serialize_result, serialize_step, serialize_usage
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
    DateConfidence,
    EventType,
    ResearchResult,
)
from aiagent.domain.usage import Pricing, Usage

CONTRACTS = Path(__file__).parents[2] / "contracts"


def load(name: str) -> dict:
    return json.loads((CONTRACTS / name).read_text())


def test_agent_produces_the_results_callback_exactly() -> None:
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

    produced = {"results": [serialize_result(r) for r in results]}

    assert produced == load("results-callback.json")


def test_agent_produces_the_failure_callback_shape() -> None:
    # HttpResultSink.report_failure posts {"error": <str>} — same shape.
    fixture = load("failure-callback.json")
    assert set(fixture.keys()) == {"error"}
    assert isinstance(fixture["error"], str)


def test_agent_consumes_the_task_request_produced_by_the_backend() -> None:
    request = TaskRequest(**load("task-request.json"))
    assert request.job_id == "3fa85f64-5717-4562-b3fc-2c963f66afa6"
    assert request.keyword == "rust hexagonal architecture"


def test_agent_produces_the_step_callback_exactly() -> None:
    step = AgentStep(
        seq=1,
        kind=AgentStepKind.SEARCH,
        detail="rust hexagonal architecture",
        reason="Start with the user's goal as the query",
        new_hits=4,
    )
    assert serialize_step(step) == load("agent-step-callback.json")


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


def test_agent_produces_the_question_callback_shape() -> None:
    # HttpResultSink.request_clarification posts {"question": <str>} (ADR-032).
    fixture = load("question-callback.json")
    assert set(fixture.keys()) == {"question"}
    assert isinstance(fixture["question"], str) and fixture["question"]


def test_agent_produces_the_usage_callback_exactly() -> None:
    # ADR-038: 8500 in-tokens * $5/MTok + 1200 out * $25/MTok + 2 * $0.008.
    usage = Usage(llm_calls=9, llm_input_tokens=8500, llm_output_tokens=1200, search_calls=2)
    pricing = Pricing(llm_input_per_mtok=5.0, llm_output_per_mtok=25.0, search_per_call=0.008)
    assert serialize_usage(usage, pricing) == load("usage-callback.json")
