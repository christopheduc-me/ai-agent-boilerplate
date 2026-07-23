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
        agent_max_steps=5,
        llm_cost_input_per_mtok=0.0,
        llm_cost_output_per_mtok=0.0,
        search_cost_per_call=0.0,
        llm_backend="anthropic",
        llm_base_url="http://localhost:11434",
    )
    base.update(overrides)
    return Settings(**base)


def test_anthropic_is_the_default_backend(monkeypatch) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")
    model = make_chat_model(settings(), max_tokens=256)

    from langchain_anthropic import ChatAnthropic

    assert isinstance(model, ChatAnthropic)
    assert model.model == "claude-opus-4-8"


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


def test_unknown_backend_fails_with_an_actionable_message() -> None:
    with pytest.raises(ValueError, match="AGENT_LLM_BACKEND"):
        make_chat_model(settings(llm_backend="mystery"), max_tokens=64)


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
