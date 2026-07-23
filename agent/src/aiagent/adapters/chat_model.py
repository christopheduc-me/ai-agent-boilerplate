"""Chat-model factory (ADR-041).

The LLM adapters in `llm.py` (enricher, policy, critic) type against
langchain's `BaseChatModel`: their prompts, defensive parsing and usage
metering are provider-agnostic. The *brand* of model — Anthropic's hosted API
or a local Ollama server — is therefore a construction detail selected here,
not a new port: adding a backend is one `elif`, never a new adapter class.
"""

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
        return ChatAnthropic(model=settings.agent_model_id, max_tokens=max_tokens)  # type: ignore[call-arg]
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
        )
    raise ValueError(
        f"unknown AGENT_LLM_BACKEND {settings.llm_backend!r} — expected one of {BACKENDS}"
    )
