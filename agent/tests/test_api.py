from fastapi.testclient import TestClient

import aiagent.adapters.api.app as api_module
from aiagent.adapters.api.app import app

client = TestClient(app)


def test_healthz() -> None:
    response = client.get("/healthz")
    assert response.status_code == 200
    assert response.json() == {"status": "ok"}


def test_enqueue_requires_internal_token(monkeypatch) -> None:
    monkeypatch.setenv("INTERNAL_API_TOKEN", "right-token")
    response = client.post(
        "/tasks",
        json={"job_id": "j1", "keyword": "k"},
        headers={"X-Internal-Token": "wrong-token"},
    )
    assert response.status_code == 401


def test_enqueue_delegates_to_celery_with_correlation_id(monkeypatch) -> None:
    monkeypatch.setenv("INTERNAL_API_TOKEN", "right-token")
    enqueued: list[tuple[str, str, str, str]] = []
    monkeypatch.setattr(
        api_module.run_research_task,
        "delay",
        lambda job_id, keyword, request_id, mode, clarification, recurring, seen_urls: (
            enqueued.append((job_id, keyword, request_id, mode))
        ),
    )

    response = client.post(
        "/tasks",
        json={"job_id": "j1", "keyword": "rust"},
        headers={"X-Internal-Token": "right-token", "X-Request-Id": "corr-42"},
    )

    assert response.status_code == 202
    assert response.json() == {"job_id": "j1", "state": "queued"}
    assert response.headers["x-request-id"] == "corr-42"
    # The mode defaults to the pre-ADR-030 workflow behaviour.
    assert enqueued == [("j1", "rust", "corr-42", "workflow")]


def test_enqueue_forwards_the_agent_mode(monkeypatch) -> None:
    monkeypatch.setenv("INTERNAL_API_TOKEN", "right-token")
    enqueued: list[str] = []
    monkeypatch.setattr(
        api_module.run_research_task,
        "delay",
        lambda job_id, keyword, request_id, mode, clarification, recurring, seen_urls: (
            enqueued.append(mode)
        ),
    )

    response = client.post(
        "/tasks",
        json={"job_id": "j1", "keyword": "rust", "mode": "agent"},
        headers={"X-Internal-Token": "right-token"},
    )

    assert response.status_code == 202
    assert enqueued == ["agent"]


def test_enqueue_falls_back_to_the_job_id_as_correlation_id(monkeypatch) -> None:
    monkeypatch.setenv("INTERNAL_API_TOKEN", "right-token")
    enqueued: list[str] = []
    monkeypatch.setattr(
        api_module.run_research_task,
        "delay",
        lambda job_id, keyword, request_id, mode, clarification, recurring, seen_urls: (
            enqueued.append(request_id)
        ),
    )

    response = client.post(
        "/tasks",
        json={"job_id": "j1", "keyword": "rust"},
        headers={"X-Internal-Token": "right-token"},
    )

    assert response.status_code == 202
    assert enqueued == ["j1"]


def test_enqueue_forwards_the_clarification(monkeypatch) -> None:
    monkeypatch.setenv("INTERNAL_API_TOKEN", "right-token")
    enqueued: list[str | None] = []
    monkeypatch.setattr(
        api_module.run_research_task,
        "delay",
        lambda job_id, keyword, request_id, mode, clarification, recurring, seen_urls: (
            enqueued.append(clarification)
        ),
    )

    client.post(
        "/tasks",
        json={"job_id": "j1", "keyword": "jaguar", "mode": "agent", "clarification": "the car"},
        headers={"X-Internal-Token": "right-token"},
    )
    client.post(
        "/tasks",
        json={"job_id": "j2", "keyword": "rust"},
        headers={"X-Internal-Token": "right-token"},
    )

    assert enqueued == ["the car", None]


def test_enqueue_forwards_the_seen_urls(monkeypatch) -> None:
    monkeypatch.setenv("INTERNAL_API_TOKEN", "right-token")
    enqueued: list[list[str]] = []
    monkeypatch.setattr(
        api_module.run_research_task,
        "delay",
        lambda job_id, keyword, request_id, mode, clarification, recurring, seen_urls: (
            enqueued.append(seen_urls)
        ),
    )

    client.post(
        "/tasks",
        json={"job_id": "j1", "keyword": "rust", "seen_urls": ["https://a"]},
        headers={"X-Internal-Token": "right-token"},
    )
    client.post(
        "/tasks",
        json={"job_id": "j2", "keyword": "rust"},
        headers={"X-Internal-Token": "right-token"},
    )

    # Default: empty memory (dispatches from pre-ADR-033 backends included).
    assert enqueued == [["https://a"], []]
