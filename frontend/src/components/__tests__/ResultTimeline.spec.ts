import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import type { SearchResult } from "@/api";
import ResultTimeline from "@/components/ResultTimeline.vue";

function result(overrides: Partial<SearchResult>): SearchResult {
  return {
    title: "a title",
    url: "https://example.com",
    snippet: "a snippet",
    published_at: "2026-06-01T00:00:00Z",
    date_confidence: "high",
    event_type: "other",
    summary: null,
    ...overrides,
  };
}

describe("ResultTimeline", () => {
  it("groups dated results by month, in the given order", () => {
    const wrapper = mount(ResultTimeline, {
      props: {
        results: [
          result({ title: "june-1", url: "https://a", published_at: "2026-06-20T00:00:00Z" }),
          result({ title: "june-2", url: "https://b", published_at: "2026-06-01T00:00:00Z" }),
          result({ title: "may", url: "https://c", published_at: "2026-05-10T00:00:00Z" }),
        ],
      },
    });

    const june = wrapper.find('[data-testid="month-June 2026"]');
    const may = wrapper.find('[data-testid="month-May 2026"]');
    expect(june.exists()).toBe(true);
    expect(may.exists()).toBe(true);
    expect(june.text()).toContain("june-1");
    expect(june.text()).toContain("june-2");
    expect(may.text()).toContain("may");
  });

  it("marks LLM-estimated dates and shows event badges", () => {
    const wrapper = mount(ResultTimeline, {
      props: {
        results: [
          result({ title: "estimated", date_confidence: "medium", event_type: "funding" }),
        ],
      },
    });

    expect(wrapper.text()).toContain("(estimated)");
    expect(wrapper.find(".badge").text()).toBe("funding");
    expect(wrapper.find(".entry").classes()).toContain("confidence-medium");
  });

  it("prefers the LLM summary over the raw snippet", () => {
    const withSummary = mount(ResultTimeline, {
      props: { results: [result({ summary: "One factual sentence." })] },
    });
    expect(withSummary.find(".summary").text()).toBe("One factual sentence.");

    const withoutSummary = mount(ResultTimeline, {
      props: { results: [result({ summary: null })] },
    });
    expect(withoutSummary.find(".summary").text()).toBe("a snippet");
  });

  it("keeps undated results in a separate section (ADR-011)", () => {
    const wrapper = mount(ResultTimeline, {
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

    const unknown = wrapper.find('[data-testid="unknown-date-section"]');
    expect(unknown.exists()).toBe(true);
    expect(unknown.text()).toContain("undated");
    expect(unknown.text()).not.toContain("dated snippet");
  });
});
