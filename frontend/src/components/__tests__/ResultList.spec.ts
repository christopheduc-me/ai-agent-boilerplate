import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import type { SearchResult } from "@/api";
import ResultList from "@/components/ResultList.vue";

function result(overrides: Partial<SearchResult>): SearchResult {
  return {
    title: "a title",
    url: "https://example.com",
    snippet: "a snippet",
    published_at: "2026-06-01T00:00:00Z",
    date_confidence: "high",
    ...overrides,
  };
}

describe("ResultList", () => {
  it("renders dated results with their publication date", () => {
    const wrapper = mount(ResultList, {
      props: {
        results: [result({ title: "dated", url: "https://a" })],
      },
    });

    expect(wrapper.text()).toContain("dated");
    expect(wrapper.text()).toContain("2026-06-01");
    expect(wrapper.find('[data-testid="unknown-date-section"]').exists()).toBe(false);
  });

  it("splits results without a date into a separate section (ADR-011)", () => {
    const wrapper = mount(ResultList, {
      props: {
        results: [
          result({ title: "dated", url: "https://a" }),
          result({
            title: "undated",
            url: "https://b",
            published_at: null,
            date_confidence: "unknown",
          }),
        ],
      },
    });

    const unknownSection = wrapper.find('[data-testid="unknown-date-section"]');
    expect(unknownSection.exists()).toBe(true);
    expect(unknownSection.text()).toContain("undated");
    expect(unknownSection.text()).not.toContain("dated snippet");
  });

  it("marks LLM-estimated dates as such", () => {
    const wrapper = mount(ResultList, {
      props: {
        results: [result({ title: "estimated", date_confidence: "medium" })],
      },
    });

    expect(wrapper.text()).toContain("(estimated)");
  });
});
