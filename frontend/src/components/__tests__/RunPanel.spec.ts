import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { SearchJobDetail } from "@/api";
import { useAuthStore } from "@/stores/auth";
import { makePinia, makeRouter } from "../../views/__tests__/helpers";
import RunPanel from "../RunPanel.vue";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return {
    ...original,
    api: { streamSearch: vi.fn(), getSearch: vi.fn(), answerSearch: vi.fn() },
  };
});

const { api } = await import("@/api");
const mocked = api as unknown as Record<
  "streamSearch" | "getSearch" | "answerSearch",
  ReturnType<typeof vi.fn>
>;

const completedAgentJob: SearchJobDetail = {
  id: "j1",
  keyword: "rust",
  mode: "agent",
  status: "completed",
  error: null,
  question: null,
  answer: null,
  recurring_search_id: null,
  usage: {
    llm_calls: 9,
    llm_input_tokens: 8500,
    llm_output_tokens: 1200,
    search_calls: 2,
    cost_usd: 0.0885,
  },
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
      is_new: true,
    },
  ],
};

async function mountView() {
  const pinia = makePinia();
  useAuthStore().token = "tok";
  const router = makeRouter();
  await router.push({ name: "searches" });
  const wrapper = mount(RunPanel, {
    props: { id: "j1" },
    global: { plugins: [pinia, router] },
  });
  await flushPromises();
  return { wrapper, router };
}

beforeEach(() => vi.clearAllMocks());

describe("RunPanel (ADR-039 inline run panel)", () => {
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

  it("shows the run's API spend (ADR-038)", async () => {
    mocked.streamSearch.mockImplementation(async (_id, _token, onUpdate) => {
      onUpdate(completedAgentJob);
    });
    const { wrapper } = await mountView();

    const cost = wrapper.find("[data-testid=job-cost]");
    expect(cost.text()).toContain("$0.0885");
    expect(cost.text()).toContain("9 LLM calls");
    expect(cost.text()).toContain("2 searches");
  });

  it("hides the cost line when nothing was spent yet", async () => {
    mocked.streamSearch.mockImplementation(async (_id, _token, onUpdate) => {
      onUpdate({
        ...completedAgentJob,
        usage: {
          llm_calls: 0,
          llm_input_tokens: 0,
          llm_output_tokens: 0,
          search_calls: 0,
          cost_usd: 0,
        },
      });
    });
    const { wrapper } = await mountView();
    expect(wrapper.find("[data-testid=job-cost]").exists()).toBe(false);
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

  it("shows the agent's question and submits the answer (ADR-032)", async () => {
    const awaiting: SearchJobDetail = {
      ...completedAgentJob,
      status: "awaiting_input",
      question: "The animal or the car?",
      steps: [],
      results: [],
    };
    // The stream stays open while the job waits (not a terminal status).
    mocked.streamSearch.mockImplementation((_id, _token, onUpdate) => {
      onUpdate(awaiting);
      return new Promise(() => {});
    });
    mocked.answerSearch.mockResolvedValue(undefined);
    mocked.getSearch.mockResolvedValue({ ...awaiting, status: "pending", answer: "the car" });
    const { wrapper } = await mountView();

    const block = wrapper.find("[data-testid=clarification-request]");
    expect(block.text()).toContain("The animal or the car?");

    await block.find("input").setValue("the car");
    await block.find("form").trigger("submit");
    await flushPromises();

    expect(mocked.answerSearch).toHaveBeenCalledWith("j1", "the car", "tok");
    // The dialog recap replaces the form once the job resumed.
    expect(wrapper.find("[data-testid=clarification-request]").exists()).toBe(false);
    expect(wrapper.find("[data-testid=clarification-recap]").text()).toContain("“the car”");
  });

  it("redirects to login when the stream answers 401", async () => {
    const { ApiError } = await import("@/api");
    mocked.streamSearch.mockRejectedValue(new ApiError(401, "expired"));
    const { router } = await mountView();
    await flushPromises();

    expect(router.currentRoute.value.name).toBe("login");
  });
});
