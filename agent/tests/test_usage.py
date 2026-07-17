"""Usage metering and pricing (ADR-038)."""

from aiagent.domain.usage import Pricing, Usage, UsageMeter

PRICING = Pricing(llm_input_per_mtok=5.0, llm_output_per_mtok=25.0, search_per_call=0.008)


def test_meter_accumulates_calls_and_tokens() -> None:
    meter = UsageMeter()
    meter.record_llm(1_000, 200)
    meter.record_llm(500, 100)
    meter.record_search()

    usage = meter.snapshot()
    assert usage == Usage(
        llm_calls=2, llm_input_tokens=1_500, llm_output_tokens=300, search_calls=1
    )


def test_cost_combines_tokens_and_searches() -> None:
    usage = Usage(llm_calls=3, llm_input_tokens=100_000, llm_output_tokens=10_000, search_calls=2)
    # 0.1 MTok * $5 + 0.01 MTok * $25 + 2 * $0.008 = 0.5 + 0.25 + 0.016
    assert usage.cost_usd(PRICING) == 0.766


def test_zero_usage_costs_nothing_and_negative_tokens_are_ignored() -> None:
    assert Usage().cost_usd(PRICING) == 0.0
    meter = UsageMeter()
    meter.record_llm(-5, -3)  # a provider quirk must never corrupt the meter
    assert meter.snapshot() == Usage(llm_calls=1)
