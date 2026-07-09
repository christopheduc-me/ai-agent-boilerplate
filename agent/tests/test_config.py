"""Fail-fast startup checks (ADR-020)."""

import pytest

from aiagent.config import forbid_placeholders, require_env


def test_require_env_passes_when_all_set(monkeypatch) -> None:
    monkeypatch.setenv("SOME_KEY", "value")
    require_env("test-component", "SOME_KEY")  # must not raise


def test_require_env_exits_on_missing_variable(monkeypatch, caplog) -> None:
    monkeypatch.delenv("SOME_KEY", raising=False)
    with pytest.raises(SystemExit) as exc:
        require_env("test-component", "SOME_KEY")
    assert exc.value.code == 1
    assert "test-component" in caplog.text
    assert "SOME_KEY" in caplog.text


def test_require_env_treats_empty_string_as_missing(monkeypatch) -> None:
    """An empty value in .env (e.g. `TAVILY_API_KEY=`) must not pass the check."""
    monkeypatch.setenv("SOME_KEY", "")
    with pytest.raises(SystemExit):
        require_env("test-component", "SOME_KEY")


def test_require_env_lists_every_missing_variable(monkeypatch, caplog) -> None:
    monkeypatch.delenv("KEY_A", raising=False)
    monkeypatch.setenv("KEY_B", "")
    with pytest.raises(SystemExit):
        require_env("test-component", "KEY_A", "KEY_B")
    assert "KEY_A" in caplog.text
    assert "KEY_B" in caplog.text


def test_forbid_placeholders_is_inert_outside_production(monkeypatch) -> None:
    monkeypatch.delenv("APP_ENV", raising=False)
    monkeypatch.setenv("SECRET", "change-me")
    forbid_placeholders("test-component", "SECRET")  # must not raise


def test_forbid_placeholders_rejects_change_me_in_production(monkeypatch, caplog) -> None:
    monkeypatch.setenv("APP_ENV", "production")
    monkeypatch.setenv("SECRET", "change-me")
    with pytest.raises(SystemExit):
        forbid_placeholders("test-component", "SECRET")
    assert "SECRET" in caplog.text


def test_forbid_placeholders_accepts_real_values_in_production(monkeypatch) -> None:
    monkeypatch.setenv("APP_ENV", "production")
    monkeypatch.setenv("SECRET", "a-real-strong-secret")
    forbid_placeholders("test-component", "SECRET")  # must not raise


def test_worker_startup_check_skips_api_keys_with_fake_providers(monkeypatch) -> None:
    """ADR-021: fake providers need no API key — the ADR-020 gate must let the
    worker start without them."""
    from aiagent.celery_app import _check_required_env

    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    _check_required_env()  # must not raise

    monkeypatch.setenv("AGENT_PROVIDERS", "live")
    with pytest.raises(SystemExit):
        _check_required_env()
