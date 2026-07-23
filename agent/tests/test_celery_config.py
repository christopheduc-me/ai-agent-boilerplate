"""Guards the reliability configuration of ADR-016 against regressions."""

from aiagent.celery_app import app
from aiagent.tasks import run_research_task


def test_tasks_are_acked_late_and_requeued_on_worker_loss() -> None:
    assert app.conf.task_acks_late is True
    assert app.conf.task_reject_on_worker_lost is True
    assert app.conf.worker_prefetch_multiplier == 1


def test_research_task_retries_with_backoff() -> None:
    assert run_research_task.max_retries == 3
    assert run_research_task.retry_backoff is True
    assert run_research_task.retry_backoff_max == 600
    assert run_research_task.retry_jitter is True


# ---------------------------------------------------------------- ADR-041


def test_ollama_backend_does_not_require_the_anthropic_key(monkeypatch) -> None:
    """Fail-fast check (ADR-020) adjusted by ADR-041: a local backend needs no
    hosted-LLM key — only the search key stays required in live mode."""
    from aiagent.celery_app import _check_required_env

    monkeypatch.setenv("AGENT_PROVIDERS", "live")
    monkeypatch.setenv("AGENT_LLM_BACKEND", "ollama")
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.setenv("TAVILY_API_KEY", "tv-key")
    _check_required_env()  # must not raise


def test_anthropic_backend_still_requires_its_key(monkeypatch) -> None:
    import pytest

    from aiagent.celery_app import _check_required_env

    monkeypatch.setenv("AGENT_PROVIDERS", "live")
    monkeypatch.setenv("AGENT_LLM_BACKEND", "anthropic")
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.setenv("TAVILY_API_KEY", "tv-key")
    with pytest.raises(SystemExit):
        _check_required_env()
