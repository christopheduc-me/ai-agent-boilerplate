"""API usage metering (ADR-038) — pure domain, no I/O.

Every paid adapter records into a `UsageMeter` owned by the task: the Claude
adapters add token counts (one `record_llm` per call), the search provider
adds `record_search`. The fakes record their calls too, with zero tokens —
so the keyless demo shows honest call counts and a $0 cost.

Pricing is injected (env-driven, see `config.Settings`): rates change and a
hardcoded per-model table would rot; the deployer sets the rates matching
their model.
"""

from dataclasses import dataclass


@dataclass(frozen=True)
class Pricing:
    """USD rates. Defaults live in `Settings` (documented in .env.example)."""

    llm_input_per_mtok: float
    llm_output_per_mtok: float
    search_per_call: float


@dataclass(frozen=True)
class Usage:
    llm_calls: int = 0
    llm_input_tokens: int = 0
    llm_output_tokens: int = 0
    search_calls: int = 0

    def cost_usd(self, pricing: Pricing) -> float:
        """Indicative spend for this usage, rounded to micro-dollars."""
        tokens = (
            self.llm_input_tokens * pricing.llm_input_per_mtok
            + self.llm_output_tokens * pricing.llm_output_per_mtok
        ) / 1_000_000
        return round(tokens + self.search_calls * pricing.search_per_call, 6)


class UsageMeter:
    """Mutable accumulator handed to the adapters; snapshot at task end."""

    def __init__(self) -> None:
        self._usage = Usage()

    def record_llm(self, input_tokens: int, output_tokens: int) -> None:
        self._usage = Usage(
            llm_calls=self._usage.llm_calls + 1,
            llm_input_tokens=self._usage.llm_input_tokens + max(input_tokens, 0),
            llm_output_tokens=self._usage.llm_output_tokens + max(output_tokens, 0),
            search_calls=self._usage.search_calls,
        )

    def record_search(self) -> None:
        self._usage = Usage(
            llm_calls=self._usage.llm_calls,
            llm_input_tokens=self._usage.llm_input_tokens,
            llm_output_tokens=self._usage.llm_output_tokens,
            search_calls=self._usage.search_calls + 1,
        )

    def snapshot(self) -> Usage:
        return self._usage


@dataclass(frozen=True)
class SpendGuard:
    """A ceiling on a run's indicative spend (ADR-048) — the money analog of the
    step budget (ADR-030). The orchestrators check it after each decision and
    degrade to a clean finish once the meter's cost crosses `cap_usd`, so a
    pathological or expensive-model run cannot burn an unbounded budget within
    the step limit. Pure: it reads the same live `UsageMeter` the adapters feed,
    so no cost is double-counted. A cap of 0 (or negative) disables it; with the
    keyless fakes pricing is $0, so the guard never trips in the demo/e2e."""

    meter: UsageMeter
    pricing: Pricing
    cap_usd: float

    def spent_usd(self) -> float:
        return self.meter.snapshot().cost_usd(self.pricing)

    def exceeded(self) -> bool:
        return self.cap_usd > 0 and self.spent_usd() >= self.cap_usd
