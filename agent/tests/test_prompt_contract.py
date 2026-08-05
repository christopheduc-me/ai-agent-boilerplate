"""Prompt guards (ADR-067) — keyless, run in CI.

The prompts are the one part of the agent no test exercised. Every unit test
drives the policy through `FakeAgentPolicy`, and so does the e2e stack
(`AGENT_PROVIDERS=fake`), so an edit that weakens an instruction leaves the
whole suite green — and defensive parsing (ADR-043) then makes the failure
silent by design, since an action the parser does not recognise degrades to
*finish*. The `ask` branch is the most exposed: it is the entire
human-in-the-loop feature (ADR-032), and the live tests (ADR-012) only cover
the `search` branch.

What can and cannot be checked without a model, deliberately:

- A prompt *lock* catches the silent edit. It cannot judge wording — it only
  makes a change impossible to merge unnoticed.
- The reply-schema description and the parser are structured, so their
  agreement on the action vocabulary is checked directly.
- Whether the wording still *works* is a question only a real model answers:
  `test_claude_policy_asks_when_the_goal_is_ambiguous` in the live suite.

A substring check on the prompt text was tried and dropped: "ask" survives in
"Never ask once a clarification is present", so the assertion passed on a
prompt with the ask instruction removed. It is left out rather than kept as
false assurance.
"""

import hashlib

from aiagent.adapters.llm import (
    CRITIQUE_PROMPT,
    ENRICHMENT_PROMPT,
    POLICY_PROMPT,
    ActionReply,
    _action_label,
    action_from_reply,
)
from aiagent.domain.models import AskAction, FinishAction, SearchAction

# sha256[:16] of each prompt. Updating these is the acknowledgement that the
# change was intentional and checked against a real model — see the failure
# message below.
PROMPT_LOCK = {
    "ENRICHMENT_PROMPT": "6473719433c0dc75",
    "POLICY_PROMPT": "c99c93b1b0054ff9",
    "CRITIQUE_PROMPT": "80b8fc8e7eea67e8",
}

LOCKED_PROMPTS = {
    "ENRICHMENT_PROMPT": ENRICHMENT_PROMPT,
    "POLICY_PROMPT": POLICY_PROMPT,
    "CRITIQUE_PROMPT": CRITIQUE_PROMPT,
}

# Every action the policy can emit, labelled by the same function the tracing
# span uses — adding a domain action forces this list, and the assertions
# below, to be updated.
EVERY_LABEL = tuple(
    _action_label(action)
    for action in (
        SearchAction(query="q", reason="r"),
        AskAction(question="q?", reason="r"),
        FinishAction(reason="r"),
    )
)


def test_prompts_are_locked() -> None:
    changed = {
        name: hashlib.sha256(text.encode()).hexdigest()[:16]
        for name, text in LOCKED_PROMPTS.items()
        if hashlib.sha256(text.encode()).hexdigest()[:16] != PROMPT_LOCK[name]
    }
    assert not changed, (
        f"prompt(s) changed: {sorted(changed)}. This test has no opinion on the new "
        "wording — it only refuses to let a prompt change through unnoticed, because "
        "nothing else in CI runs a real model. Re-run the live suite "
        "(`RUN_LIVE_TESTS=1 uv run pytest tests/test_live_providers.py`) to confirm "
        "each branch still behaves, then update PROMPT_LOCK to: "
        f"{changed}"
    )


def test_action_reply_schema_documents_every_action() -> None:
    """Structured output shows this description to the model, so a gap here has
    the same effect as a gap in the prompt."""
    description = (ActionReply.model_fields["action"].description or "").lower()

    missing = sorted(label for label in EVERY_LABEL if label not in description)
    assert not missing, f"ActionReply.action no longer documents {missing}"


def test_every_action_label_round_trips_through_the_parser() -> None:
    """The labels are only meaningful if `action_from_reply` accepts them: a
    rename on one side and not the other degrades to FINISH, silently."""
    assert action_from_reply(ActionReply(action="search", query="q", reason="r")) == SearchAction(
        query="q", reason="r"
    )
    assert action_from_reply(ActionReply(action="ask", question="q?", reason="r")) == AskAction(
        question="q?", reason="r"
    )
    assert action_from_reply(ActionReply(action="finish", reason="r")) == FinishAction(reason="r")
