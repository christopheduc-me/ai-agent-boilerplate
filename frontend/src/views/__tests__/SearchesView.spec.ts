import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useAuthStore } from "@/stores/auth";
import SearchesView from "../SearchesView.vue";
import { makePinia, makeRouter } from "./helpers";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return {
    ...original,
    api: {
      listSearches: vi.fn(),
      launchSearch: vi.fn(),
      listRecurring: vi.fn(),
      createRecurring: vi.fn(),
      deleteRecurring: vi.fn(),
    },
  };
});

const { api } = await import("@/api");
const mocked = api as unknown as Record<
  "listSearches" | "launchSearch" | "listRecurring" | "createRecurring" | "deleteRecurring",
  ReturnType<typeof vi.fn>
>;

async function mountView() {
  const pinia = makePinia();
  useAuthStore().token = "tok";
  const router = makeRouter();
  await router.push({ name: "searches" });
  const wrapper = mount(SearchesView, {
    global: { plugins: [pinia, router], stubs: { RunPanel: true } },
  });
  await flushPromises();
  return { wrapper, router };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocked.listSearches.mockResolvedValue([]);
  mocked.listRecurring.mockResolvedValue([]);
});

describe("SearchesView (single-page workbench, ADR-039)", () => {
  it("launches the workflow demo inline — no navigation", async () => {
    mocked.launchSearch.mockResolvedValue({ job_id: "j1" });
    const { wrapper, router } = await mountView();

    expect(wrapper.find("[data-testid=empty-stage]").exists()).toBe(true);

    const demo = wrapper.find("[data-testid=workflow-demo]");
    await demo.find("input").setValue("rust");
    await demo.trigger("submit");
    await flushPromises();

    expect(mocked.launchSearch).toHaveBeenCalledWith("rust", "tok", "workflow");
    // ADR-039: the run plays out in place — same route, panel mounted.
    expect(router.currentRoute.value.name).toBe("searches");
    expect(wrapper.find("run-panel-stub").attributes("id")).toBe("j1");
    expect(wrapper.find("[data-testid=empty-stage]").exists()).toBe(false);
  });

  it("launches the agent demo in agent mode", async () => {
    mocked.launchSearch.mockResolvedValue({ job_id: "j2" });
    const { wrapper } = await mountView();

    const demo = wrapper.find("[data-testid=agent-demo]");
    await demo.find("input").setValue("rust news");
    await demo.trigger("submit");
    await flushPromises();

    expect(mocked.launchSearch).toHaveBeenCalledWith("rust news", "tok", "agent");
  });

  it("lists previous searches with their mode badge and total spend", async () => {
    const usage = (cost: number) => ({
      llm_calls: 3,
      llm_input_tokens: 100,
      llm_output_tokens: 10,
      search_calls: 1,
      cost_usd: cost,
    });
    const twoHoursAgo = new Date(Date.now() - 2 * 3600_000).toISOString();
    mocked.listSearches.mockResolvedValue([
      {
        id: "j1",
        keyword: "one",
        mode: "workflow",
        status: "completed",
        usage: usage(0.01),
        created_at: twoHoursAgo,
      },
      {
        id: "j2",
        keyword: "two",
        mode: "agent",
        status: "running",
        usage: usage(0.0223),
        created_at: twoHoursAgo,
      },
    ]);
    const { wrapper } = await mountView();

    const items = wrapper.findAll(".history li");
    expect(items).toHaveLength(2);
    expect(items[1].text()).toContain("two");
    expect(items[1].find(".mode").attributes("data-mode")).toBe("agent");
    // Status is a colored pill, same component as the run panel.
    expect(items[1].find(".status").attributes("data-status")).toBe("running");
    // Launch time, relative — the API's created_at surfaced at last.
    expect(items[1].text()).toContain("2 h ago");
    // Picking a past run loads it in the panel (ADR-039), no navigation.
    await items[1].find("button.job-link").trigger("click");
    expect(wrapper.find("run-panel-stub").attributes("id")).toBe("j2");
    // Spend tracking (ADR-038): per-job cost and the sum of all runs.
    expect(items[1].text()).toContain("$0.0223");
    expect(wrapper.find("[data-testid=total-cost]").text()).toContain("$0.0323");
  });

  it("makes a job waiting on the user stand out in the history (ADR-032)", async () => {
    mocked.listSearches.mockResolvedValue([
      {
        id: "j3",
        keyword: "ambiguous goal",
        mode: "agent",
        status: "awaiting_input",
        usage: { llm_calls: 1, llm_input_tokens: 5, llm_output_tokens: 5, search_calls: 0, cost_usd: 0.001 },
        created_at: new Date().toISOString(),
      },
    ]);
    const { wrapper } = await mountView();

    const row = wrapper.find(".history li");
    expect(row.classes()).toContain("attention");
    expect(row.find(".status").text()).toContain("needs your answer");
  });

  it("creates and deletes a recurring search (ADR-033)", async () => {
    mocked.createRecurring.mockResolvedValue({ id: "r1" });
    mocked.deleteRecurring.mockResolvedValue(undefined);
    mocked.listRecurring
      .mockResolvedValueOnce([]) // initial mount
      .mockResolvedValue([
        {
          id: "r1",
          keyword: "rust releases",
          mode: "agent",
          interval_minutes: 60,
          webhook_url: "https://hooks.example.com/digest",
          created_at: "2026-07-01T00:00:00Z",
          last_run_at: null,
        },
      ]);
    const { wrapper } = await mountView();

    const form = wrapper.find("[data-testid=recurring-form]");
    await form.find("input").setValue("rust releases");
    await form.find("select").setValue("agent");
    await form.find("input[type=number]").setValue(60);
    await form.trigger("submit");
    await flushPromises();

    expect(mocked.createRecurring).toHaveBeenCalledWith("rust releases", "agent", 60, "tok", "");
    const item = wrapper.find("[data-testid=recurring-r1]");
    expect(item.text()).toContain("rust releases");
    expect(item.text()).toContain("first run pending");
    expect(item.text()).toContain("digest webhook"); // ADR-036

    await item.find("button.delete").trigger("click");
    await flushPromises();
    expect(mocked.deleteRecurring).toHaveBeenCalledWith("r1", "tok");
  });

  it("links the ops consoles on the current host (ADR-040)", async () => {
    const { wrapper } = await mountView();

    const ops = wrapper.find("[data-testid=ops-consoles]");
    const hrefs = ops.findAll("a").map((a) => a.attributes("href"));
    // jsdom serves the tests from localhost — the links must follow the host.
    expect(hrefs).toContain("http://localhost:5555"); // Flower (Celery workers)
    expect(hrefs).toContain("http://localhost:16686"); // Jaeger (traces)
  });

  it("shows quota errors from the backend (ADR-017)", async () => {
    const { ApiError } = await import("@/api");
    mocked.launchSearch.mockRejectedValue(new ApiError(429, "daily search quota reached"));
    const { wrapper } = await mountView();

    const demo = wrapper.find("[data-testid=workflow-demo]");
    await demo.find("input").setValue("rust");
    await demo.trigger("submit");
    await flushPromises();

    expect(wrapper.find(".error").text()).toContain("quota");
  });
});
