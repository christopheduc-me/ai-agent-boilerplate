"""The Celery task end to end (ADR-021 fake providers + mocked backend callbacks).

The task object is called directly (synchronously, outside a worker): Celery
then re-raises exceptions instead of scheduling retries, which is exactly what
these tests assert.
"""

import httpx
import pytest
import respx
from pydantic import ValidationError

from aiagent.tasks import run_research_task

BACKEND = "http://backend-test:8000"


@pytest.fixture()
def fake_env(monkeypatch) -> None:
    monkeypatch.setenv("BACKEND_INTERNAL_URL", BACKEND)
    monkeypatch.setenv("INTERNAL_API_TOKEN", "test-token")
    monkeypatch.setenv("AGENT_PROVIDERS", "fake")


@respx.mock
def test_task_runs_end_to_end_with_fake_providers(fake_env) -> None:
    started = respx.post(f"{BACKEND}/internal/jobs/job-1/started").mock(
        return_value=httpx.Response(204)
    )
    results = respx.post(f"{BACKEND}/internal/jobs/job-1/results").mock(
        return_value=httpx.Response(204)
    )

    count = run_research_task("job-1", "keyword", request_id="corr-1")

    assert count == 4  # the four deterministic fake hits
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
