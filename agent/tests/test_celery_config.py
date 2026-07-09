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
