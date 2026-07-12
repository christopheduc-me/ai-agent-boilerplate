import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { SearchJobDetail } from "@/api";
import { useAuthStore } from "@/stores/auth";
import SearchDetailView from "../SearchDetailView.vue";
import { makePinia, makeRouter } from "./helpers";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return { ...original, api: { streamSearch: vi.fn(), getSearch: vi.fn() } };
});

const { api } = await import("@/api");
const mocked = api as unknown as Record<"streamSearch" | "getSearch", ReturnType<typeof vi.fn>>;

const completedAgentJob: SearchJobDetail = {
  id: "j1",
  keyword: "rust",
  mode: "agent",
  status: "completed",
  error: null,
  created_at: "2026-07-01T00:00:00Z",
  completed_at: "2026-07-01T00:00:10Z",
  steps: [
    { seq: 1, kind: "search", detail: "rust", reason: "Start with the goal", new_hits: 2 },
    { seq: 2, kind: "finish", detail: "", reason: "Coverage looks sufficient", new_hits: 0 },
  ],
  results: [
    {
      title: "hit",
      url: "https://example.com",
      snippet: "s",
      published_at: "2026-06-01T00:00:00Z",
      date_confidence: "high",
      event_type: "release",
      summary: "A release.",
    },
  ],
};

async function mountView() {
  const pinia = makePinia();
  useAuthStore().token = "tok";
  const router = makeRouter();
  await router.push({ name: "search-detail", params: { id: "j1" } });
  const wrapper = mount(SearchDetailView, {
    props: { id: "j1" },
    global: { plugins: [pinia, router] },
  });
  await flushPromises();
  return { wrapper, router };
}

beforeEach(() => vi.clearAllMocks());

describe("SearchDetailView", () => {
  it("renders the journal and the timeline from SSE updates (ADR-026/030)", async () => {
    mocked.streamSearch.mockImplementation(async (_id, _token, onUpdate) => {
      onUpdate(completedAgentJob);
    });
    const { wrapper } = await mountView();

    expect(wrapper.find("h2").text()).toBe("“rust”");
    const journal = wrapper.find("[data-testid=agent-journal]");
    expect(journal.exists()).toBe(true);
    expect(journal.text()).toContain("Coverage looks sufficient");
    // Terminal status: no pulsing "thinking" indicator.
    expect(wrapper.find("[data-testid=agent-thinking]").exists()).toBe(false);
    expect(wrapper.find(".timeline").exists()).toBe(true);
  });

  it("hides the journal for workflow jobs", async () => {
    mocked.streamSearch.mockImplementation(async (_id, _token, onUpdate) => {
      onUpdate({ ...completedAgentJob, mode: "workflow", steps: [] });
    });
    const { wrapper } = await mountView();

    expect(wrapper.find("[data-testid=agent-journal]").exists()).toBe(false);
    expect(wrapper.find(".timeline").exists()).toBe(true);
  });

  it("falls back to polling when the stream fails (ADR-026)", async () => {
    vi.useFakeTimers();
    try {
      mocked.streamSearch.mockRejectedValue(new Error("stream down"));
      mocked.getSearch.mockResolvedValue(completedAgentJob);
      const { wrapper } = await mountView();

      await vi.runOnlyPendingTimersAsync();
      await flushPromises();

      expect(mocked.getSearch).toHaveBeenCalled();
      expect(wrapper.find(".timeline").exists()).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("redirects to login when the stream answers 401", async () => {
    const { ApiError } = await import("@/api");
    mocked.streamSearch.mockRejectedValue(new ApiError(401, "expired"));
    const { router } = await mountView();
    await flushPromises();

    expect(router.currentRoute.value.name).toBe("login");
  });
});
