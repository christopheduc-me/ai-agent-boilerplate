"""LLM fallback chain (ADR-052): the primary model is tried first; if it errors
(provider down/quota), LangChain fallbacks try the next model. Driven with fake
models whose structured output is a real `RunnableLambda`, so the actual
`with_fallbacks` mechanism is exercised — no network."""

from langchain_core.runnables import RunnableLambda

from aiagent.adapters.llm import ActionReply, LlmAgentPolicy, structured_with_fallbacks
from aiagent.domain.models import SearchAction


class _Raw:
    """Quacks like an AIMessage: text content + usage metadata (ADR-038)."""

    content = ""
    usage_metadata = {"input_tokens": 3, "output_tokens": 1}


class FakeModel:
    """A chat model whose `with_structured_output` returns a real Runnable that
    either raises (simulating an outage) or yields a structured reply."""

    def __init__(self, reply: object = None, error: Exception | None = None) -> None:
        self._reply = reply
        self._error = error

    def with_structured_output(self, schema: object, include_raw: bool = False) -> RunnableLambda:
        def run(_prompt: object) -> dict:
            if self._error is not None:
                raise self._error
            return {"raw": _Raw(), "parsed": self._reply}

        return RunnableLambda(run)


def test_single_model_has_no_fallback_wrapper() -> None:
    reply = ActionReply(action="finish", reason="ok")
    chain = structured_with_fallbacks([FakeModel(reply=reply)], ActionReply)
    assert chain.invoke("p")["parsed"] == reply


def test_secondary_model_is_used_when_the_primary_errors() -> None:
    reply = ActionReply(action="finish", reason="from fallback")
    chain = structured_with_fallbacks(
        [FakeModel(error=RuntimeError("provider down")), FakeModel(reply=reply)], ActionReply
    )
    assert chain.invoke("p")["parsed"] == reply


def test_policy_adapter_falls_back_to_the_secondary_model() -> None:
    reply = ActionReply(action="search", query="q", reason="r")
    policy = LlmAgentPolicy(
        FakeModel(error=RuntimeError("primary down")),  # type: ignore[arg-type]
        fallbacks=[FakeModel(reply=reply)],  # type: ignore[list-item]
    )
    action = policy.decide("goal", [], [])
    assert isinstance(action, SearchAction) and action.query == "q"
