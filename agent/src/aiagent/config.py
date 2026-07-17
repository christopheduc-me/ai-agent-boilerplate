"""Environment-driven configuration (12-factor, ADR-014) + startup checks (ADR-020)."""

import logging
import os
from dataclasses import dataclass

logger = logging.getLogger(__name__)

#: Placeholder values that must never survive into production.
_PLACEHOLDERS = frozenset({"change-me"})


def require_env(component: str, *names: str) -> None:
    """Fail-fast startup check (ADR-020): every listed variable must be set and
    non-empty, otherwise the process logs the exact gap and refuses to start —
    instead of failing later on the first task/request."""
    missing = sorted(name for name in names if not os.environ.get(name))
    if missing:
        logger.error(
            "%s cannot start: missing required environment variable(s): %s (see .env.example)",
            component,
            ", ".join(missing),
        )
        raise SystemExit(1)


def forbid_placeholders(component: str, *names: str) -> None:
    """Production-only guard (APP_ENV=production): a secret left at its
    development placeholder is treated as missing."""
    if os.environ.get("APP_ENV") != "production":
        return
    bad = sorted(name for name in names if os.environ.get(name, "") in _PLACEHOLDERS)
    if bad:
        logger.error(
            "%s cannot start in production: environment variable(s) still set "
            "to a development placeholder: %s",
            component,
            ", ".join(bad),
        )
        raise SystemExit(1)


@dataclass(frozen=True)
class Settings:
    redis_url: str
    backend_internal_url: str
    internal_api_token: str
    agent_model_id: str
    # "live" (Tavily + Claude) or "fake" (deterministic in-process adapters,
    # no API key needed — e2e tests and keyless local development, ADR-021).
    providers: str
    # Step budget of the agentic loop (ADR-030) — the cost guard: each step is
    # at most one policy LLM call plus one provider search.
    agent_max_steps: int
    # Indicative pricing (ADR-038), USD; set the rates matching your model and
    # search plan. Defaults documented in .env.example.
    llm_cost_input_per_mtok: float
    llm_cost_output_per_mtok: float
    search_cost_per_call: float

    @classmethod
    def from_env(cls) -> "Settings":
        return cls(
            redis_url=os.environ.get("REDIS_URL", "redis://localhost:6379/0"),
            backend_internal_url=os.environ.get("BACKEND_INTERNAL_URL", "http://localhost:8000"),
            internal_api_token=os.environ.get("INTERNAL_API_TOKEN", "change-me"),
            agent_model_id=os.environ.get("AGENT_MODEL_ID", "claude-opus-4-8"),
            providers=os.environ.get("AGENT_PROVIDERS", "live"),
            agent_max_steps=int(os.environ.get("AGENT_MAX_STEPS", "5")),
            llm_cost_input_per_mtok=float(os.environ.get("LLM_COST_INPUT_PER_MTOK", "5.0")),
            llm_cost_output_per_mtok=float(os.environ.get("LLM_COST_OUTPUT_PER_MTOK", "25.0")),
            search_cost_per_call=float(os.environ.get("SEARCH_COST_PER_CALL", "0.008")),
        )
