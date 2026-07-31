"""Chat-model factory (ADR-041).

The LLM adapters in `llm.py` (enricher, policy, critic) type against
langchain's `BaseChatModel`: their prompts, defensive parsing and usage
metering are provider-agnostic. The *brand* of model — Anthropic's hosted API
or a local Ollama server — is therefore a construction detail selected here,
not a new port: adding a backend is one `elif`, never a new adapter class.
"""

import dataclasses
from typing import TYPE_CHECKING

from aiagent.config import Settings

if TYPE_CHECKING:
    from langchain_core.language_models import BaseChatModel

BACKENDS = ("anthropic", "ollama")


def make_chat_model(settings: Settings, max_tokens: int) -> "BaseChatModel":
    """Builds the chat model the live adapters talk to, from AGENT_LLM_BACKEND."""
    if settings.llm_backend == "anthropic":
        from langchain_anthropic import ChatAnthropic

        # `model` / `max_tokens` are pydantic aliases mypy cannot see.
        return ChatAnthropic(  # type: ignore[call-arg]
            model=settings.agent_model_id,
            max_tokens=max_tokens,
            # Per-call hardening (ADR-044).
            default_request_timeout=settings.llm_timeout_seconds,
            max_retries=settings.llm_max_retries,
        )
    if settings.llm_backend == "ollama":
        from langchain_ollama import ChatOllama

        return ChatOllama(
            model=settings.agent_model_id,
            base_url=settings.llm_base_url,
            num_predict=max_tokens,
            # Thinking models (gemma4, deepseek-r1…) otherwise burn the whole
            # num_predict budget on hidden reasoning and return empty content
            # for these short strict-JSON tasks.
            reasoning=False,
            # ChatOllama has no direct timeout; it reaches the underlying http
            # client through client_kwargs (ADR-044). Ollama has no built-in
            # retry — a failed call falls back to the Celery task retry.
            client_kwargs={"timeout": settings.llm_timeout_seconds},
        )
    raise ValueError(
        f"unknown AGENT_LLM_BACKEND {settings.llm_backend!r} — expected one of {BACKENDS}"
    )


def make_fallback_chat_models(settings: Settings, max_tokens: int) -> list["BaseChatModel"]:
    """Builds the fallback models from `AGENT_MODEL_FALLBACKS` (ADR-052): each
    `backend:model_id` spec is a model to try, in order, when the primary
    errors. Empty by default. Splits on the first colon so Ollama tags
    (`qwen3:14b`) survive — the same spec grammar as the eval harness (ADR-045)."""
    models: list[BaseChatModel] = []
    for spec in settings.model_fallbacks:
        backend, _, model_id = spec.partition(":")
        if not model_id:
            raise ValueError(
                f"bad AGENT_MODEL_FALLBACKS spec {spec!r} — expected 'backend:model_id'"
            )
        replaced = dataclasses.replace(settings, llm_backend=backend, agent_model_id=model_id)
        models.append(make_chat_model(replaced, max_tokens))
    return models
