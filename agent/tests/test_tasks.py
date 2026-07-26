"""The Celery task end to end (ADR-021 fake providers + mocked backend callbacks).

The task object is called directly (synchronously, outside a worker): Celery
then re-raises exceptions instead of scheduling retries, which is exactly what
these tests assert.
"""

import json

import httpx
import pytest
import respx
from pydantic import ValidationError

from aiagent.tasks import run_research_task

BACKEND = "http://backend-test:8000"


@pytest.fixture(params=["loop", "langgraph"])
def fake_env(request, monkeypatch) -> None:
    """Exercises both agent orchestrators (ADR-046). For langgraph, the durable
    Redis checkpointer is swapped for a per-test in-memory one — same graph, no
    Redis in unit tests; the real RedisSaver path is covered by the live e2e."""
    monkeypatch.setenv("BACKEND_INTERNAL_URL", BACKEND)
    monkeypatch.setenv("INTERNAL_API_TOKEN", "test-token")
    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    monkeypatch.setenv("AGENT_ORCHESTRATOR", request.param)
    if request.param == "langgraph":
        from contextlib import contextmanager

        from langgraph.checkpoint.memory import InMemorySaver

        import aiagent.tasks as tasks_mod

        # One saver shared across calls in a test, so a HITL resume finds the
        # checkpoint left by the paused run (mirrors durable Redis across tasks).
        saver = InMemorySaver()

        @contextmanager
        def _mem_checkpointer(_settings):  # type: ignore[no-untyped-def]
            yield saver

        monkeypatch.setattr(tasks_mod, "_agent_checkpointer", _mem_checkpointer)


@respx.mock
def test_task_runs_end_to_end_with_fake_providers(fake_env) -> None:
    started = respx.post(f"{BACKEND}/internal/jobs/job-1/started").mock(
        return_value=httpx.Response(204)
    )
    results = respx.post(f"{BACKEND}/internal/jobs/job-1/results").mock(
        return_value=httpx.Response(204)
    )

    count = run_research_task("job-1", "keyword", request_id="corr-1")

    assert count == 5  # the five deterministic fake hits
    assert started.called
    assert results.called
    # Correlation (ADR-018) rides on every callback.
    assert results.calls.last.request.headers["x-request-id"] == "corr-1"


@respx.mock
def test_task_defaults_the_correlation_id_to_the_job_id(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-2/started").mock(return_value=httpx.Response(204))
    results = respx.post(f"{BACKEND}/internal/jobs/job-2/results").mock(
        return_value=httpx.Response(204)
    )

    run_research_task("job-2", "keyword")

    assert results.calls.last.request.headers["x-request-id"] == "job-2"


@respx.mock
def test_misconfiguration_is_reported_as_a_failed_job_and_raises(fake_env, monkeypatch) -> None:
    monkeypatch.setenv("AGENT_PROVIDERS", "live")
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    failure = respx.post(f"{BACKEND}/internal/jobs/job-3/failure").mock(
        return_value=httpx.Response(204)
    )

    with pytest.raises(ValidationError):
        run_research_task("job-3", "keyword")

    assert failure.called
    assert b"agent misconfigured" in failure.calls.last.request.content


@respx.mock
def test_delivery_failure_reports_and_raises_for_celery_retry(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-4/started").mock(return_value=httpx.Response(204))
    respx.post(f"{BACKEND}/internal/jobs/job-4/results").mock(return_value=httpx.Response(500))
    failure = respx.post(f"{BACKEND}/internal/jobs/job-4/failure").mock(
        return_value=httpx.Response(204)
    )

    with pytest.raises(httpx.HTTPStatusError):
        run_research_task("job-4", "keyword")

    # Best-effort failure report before the exception propagates (ADR-016).
    assert failure.called


@respx.mock
def test_agent_mode_runs_the_loop_and_reports_the_journal(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-5/started").mock(return_value=httpx.Response(204))
    steps = respx.post(f"{BACKEND}/internal/jobs/job-5/steps").mock(
        return_value=httpx.Response(204)
    )
    results = respx.post(f"{BACKEND}/internal/jobs/job-5/results").mock(
        return_value=httpx.Response(204)
    )

    count = run_research_task("job-5", "keyword", mode="agent")

    # Fake policy: search -> refine (0 new, dedup) -> finish (ADR-030).
    assert count == 5
    assert steps.call_count == 4
    assert results.called
    import json as _json

    kinds = [_json.loads(c.request.content)["kind"] for c in steps.calls]
    assert kinds == ["search", "search", "finish", "critique"]


@respx.mock
def test_agent_mode_pauses_on_an_ambiguous_goal_and_resumes_with_the_answer(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-6/started").mock(return_value=httpx.Response(204))
    question = respx.post(f"{BACKEND}/internal/jobs/job-6/question").mock(
        return_value=httpx.Response(204)
    )
    steps = respx.post(f"{BACKEND}/internal/jobs/job-6/steps").mock(
        return_value=httpx.Response(204)
    )
    results = respx.post(f"{BACKEND}/internal/jobs/job-6/results").mock(
        return_value=httpx.Response(204)
    )

    # First run: the fake policy asks, the job pauses, nothing is delivered.
    count = run_research_task("job-6", "ambiguous topic", mode="agent")
    assert count == 0
    assert question.called and not results.called
    assert json.loads(question.calls.last.request.content)["question"].startswith("Your goal")

    # Re-dispatch with the user's answer: the loop runs to completion.
    count = run_research_task("job-6", "ambiguous topic", mode="agent", clarification="cars")
    assert count == 5
    assert results.called
    kinds = [json.loads(c.request.content)["kind"] for c in steps.calls]
    assert kinds == ["search", "search", "finish", "critique"]


@respx.mock
def test_recurring_run_flags_the_delta_and_reports_it(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-7/started").mock(return_value=httpx.Response(204))
    steps = respx.post(f"{BACKEND}/internal/jobs/job-7/steps").mock(
        return_value=httpx.Response(204)
    )
    results = respx.post(f"{BACKEND}/internal/jobs/job-7/results").mock(
        return_value=httpx.Response(204)
    )

    # The memory covers two of the five fake URLs (ADR-033).
    seen = ["https://example.com/old", "https://example.com/recent"]
    count = run_research_task("job-7", "keyword", mode="agent", recurring=True, seen_urls=seen)

    assert count == 5
    payload = json.loads(results.calls.last.request.content)
    by_url = {r["url"]: r["is_new"] for r in payload["results"]}
    assert by_url["https://example.com/old"] is False
    assert by_url["https://example.com/recent"] is False
    assert by_url["https://example.com/llm"] is True
    assert by_url["https://example.com/page"] is True
    # Journal: search, search, finish, critique, then the delta report.
    kinds = [json.loads(c.request.content)["kind"] for c in steps.calls]
    assert kinds == ["search", "search", "finish", "critique", "report"]
    assert json.loads(steps.calls.last.request.content)["new_hits"] == 3


@respx.mock
def test_usage_is_reported_at_task_end(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-8/started").mock(return_value=httpx.Response(204))
    respx.post(f"{BACKEND}/internal/jobs/job-8/steps").mock(return_value=httpx.Response(204))
    respx.post(f"{BACKEND}/internal/jobs/job-8/results").mock(return_value=httpx.Response(204))
    usage = respx.post(f"{BACKEND}/internal/jobs/job-8/usage").mock(
        return_value=httpx.Response(204)
    )

    run_research_task("job-8", "keyword", mode="agent")

    # Fake mode (ADR-038): every call counted, zero tokens, zero cost —
    # enricher x5 + policy x3 + critic x1 = 9 LLM calls; 2 searches.
    payload = json.loads(usage.calls.last.request.content)
    assert payload == {
        "llm_calls": 9,
        "llm_input_tokens": 0,
        "llm_output_tokens": 0,
        "search_calls": 2,
        "cost_usd": 0.0,
    }


@respx.mock
def test_usage_is_reported_even_when_the_run_pauses(fake_env) -> None:
    respx.post(f"{BACKEND}/internal/jobs/job-9/started").mock(return_value=httpx.Response(204))
    respx.post(f"{BACKEND}/internal/jobs/job-9/question").mock(return_value=httpx.Response(204))
    usage = respx.post(f"{BACKEND}/internal/jobs/job-9/usage").mock(
        return_value=httpx.Response(204)
    )

    run_research_task("job-9", "ambiguous topic", mode="agent")

    # The ask decision was one policy call: it cost something, it is reported.
    payload = json.loads(usage.calls.last.request.content)
    assert payload["llm_calls"] == 1
    assert payload["search_calls"] == 0
