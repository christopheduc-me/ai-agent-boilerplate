"""Chat-model factory (ADR-041): the LLM brand (Anthropic API or a local
Ollama server) is a construction detail inside the adapters — selected by
environment, never a new port."""

import pytest

from aiagent.adapters.chat_model import make_chat_model
from aiagent.config import Settings


def settings(**overrides) -> Settings:
    base = dict(
        redis_url="redis://x",
        backend_internal_url="http://x",
        internal_api_token="t",
        agent_model_id="claude-opus-4-8",
        providers="live",
        search_providers=["tavily"],
        agent_max_steps=5,
        agent_max_cost_usd=2.0,
        agent_orchestrator="langgraph",
        llm_cost_input_per_mtok=0.0,
        llm_cost_output_per_mtok=0.0,
        search_cost_per_call=0.0,
        llm_backend="anthropic",
        llm_base_url="http://localhost:11434",
        llm_timeout_seconds=60.0,
        llm_max_retries=2,
        model_fallbacks=[],
    )
    base.update(overrides)
    return Settings(**base)


def test_anthropic_is_the_default_backend(monkeypatch) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")
    model = make_chat_model(settings(), max_tokens=256)

    from langchain_anthropic import ChatAnthropic

    assert isinstance(model, ChatAnthropic)
    assert model.model == "claude-opus-4-8"


def test_anthropic_gets_the_timeout_and_retries(monkeypatch) -> None:
    # ADR-044: a hung or flaky provider call must not stall the worker until
    # Celery's coarse retry; the client bounds it directly.
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")
    model = make_chat_model(settings(llm_timeout_seconds=42.0, llm_max_retries=4), max_tokens=256)

    assert model.default_request_timeout == 42.0
    assert model.max_retries == 4


def test_ollama_backend_targets_the_configured_local_server() -> None:
    model = make_chat_model(
        settings(
            llm_backend="ollama",
            agent_model_id="qwen3:14b",
            llm_base_url="http://host.docker.internal:11434",
        ),
        max_tokens=256,
    )

    from langchain_ollama import ChatOllama

    assert isinstance(model, ChatOllama)
    assert model.model == "qwen3:14b"
    assert model.base_url == "http://host.docker.internal:11434"
    # max_tokens must reach Ollama too (its name for it is num_predict).
    assert model.num_predict == 256


def test_ollama_gets_the_timeout_through_client_kwargs() -> None:
    # ADR-044: ChatOllama has no direct timeout — it reaches the underlying
    # http client via client_kwargs. Retries stay with Celery (Ollama has none).
    model = make_chat_model(settings(llm_backend="ollama", llm_timeout_seconds=90.0), max_tokens=64)

    assert model.client_kwargs == {"timeout": 90.0}


def test_unknown_backend_fails_with_an_actionable_message() -> None:
    with pytest.raises(ValueError, match="AGENT_LLM_BACKEND"):
        make_chat_model(settings(llm_backend="mystery"), max_tokens=64)


def test_fallback_models_are_built_from_the_specs() -> None:
    # ADR-052: `backend:model_id`, split on the first colon (Ollama tags keep it).
    from langchain_ollama import ChatOllama

    from aiagent.adapters.chat_model import make_fallback_chat_models

    models = make_fallback_chat_models(
        settings(model_fallbacks=["ollama:qwen3:14b", "ollama:gemma:2b"]), max_tokens=64
    )
    assert [type(m).__name__ for m in models] == ["ChatOllama", "ChatOllama"]
    assert isinstance(models[0], ChatOllama) and models[0].model == "qwen3:14b"


def test_no_fallbacks_by_default() -> None:
    from aiagent.adapters.chat_model import make_fallback_chat_models

    assert make_fallback_chat_models(settings(), max_tokens=64) == []


def test_fallback_spec_without_a_model_is_rejected() -> None:
    from aiagent.adapters.chat_model import make_fallback_chat_models

    with pytest.raises(ValueError, match="AGENT_MODEL_FALLBACKS"):
        make_fallback_chat_models(settings(model_fallbacks=["ollama"]), max_tokens=64)


def test_settings_read_the_model_fallbacks(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_MODEL_FALLBACKS", "anthropic:claude-haiku-4-5, ollama:qwen3:14b")
    assert Settings.from_env().model_fallbacks == ["anthropic:claude-haiku-4-5", "ollama:qwen3:14b"]


def test_settings_default_model_fallbacks_empty(monkeypatch) -> None:
    monkeypatch.delenv("AGENT_MODEL_FALLBACKS", raising=False)
    assert Settings.from_env().model_fallbacks == []


def test_settings_default_to_anthropic(monkeypatch) -> None:
    monkeypatch.delenv("AGENT_LLM_BACKEND", raising=False)
    monkeypatch.delenv("AGENT_LLM_BASE_URL", raising=False)
    s = Settings.from_env()
    assert s.llm_backend == "anthropic"
    assert s.llm_base_url == "http://localhost:11434"


def test_settings_read_the_backend_from_env(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_LLM_BACKEND", "ollama")
    monkeypatch.setenv("AGENT_LLM_BASE_URL", "http://gpu-box:11434")
    s = Settings.from_env()
    assert s.llm_backend == "ollama"
    assert s.llm_base_url == "http://gpu-box:11434"


def test_settings_default_the_timeout_and_retries(monkeypatch) -> None:
    monkeypatch.delenv("AGENT_LLM_TIMEOUT_SECONDS", raising=False)
    monkeypatch.delenv("AGENT_LLM_MAX_RETRIES", raising=False)
    s = Settings.from_env()
    assert s.llm_timeout_seconds == 60.0
    assert s.llm_max_retries == 2


def test_settings_read_the_timeout_and_retries_from_env(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_LLM_TIMEOUT_SECONDS", "120.5")
    monkeypatch.setenv("AGENT_LLM_MAX_RETRIES", "0")
    s = Settings.from_env()
    assert s.llm_timeout_seconds == 120.5
    assert s.llm_max_retries == 0


def test_settings_default_the_spend_cap(monkeypatch) -> None:
    monkeypatch.delenv("AGENT_MAX_COST_USD", raising=False)
    assert Settings.from_env().agent_max_cost_usd == 2.0


def test_settings_read_the_spend_cap_from_env(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_MAX_COST_USD", "0")  # 0 disables the cap
    assert Settings.from_env().agent_max_cost_usd == 0.0
