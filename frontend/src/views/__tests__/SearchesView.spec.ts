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
  const wrapper = mount(SearchesView, { global: { plugins: [pinia, router] } });
  await flushPromises();
  return { wrapper, router };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocked.listSearches.mockResolvedValue([]);
  mocked.listRecurring.mockResolvedValue([]);
});

describe("SearchesView (two demos, ADR-030)", () => {
  it("launches the workflow demo in workflow mode", async () => {
    mocked.launchSearch.mockResolvedValue({ job_id: "j1" });
    const { wrapper, router } = await mountView();

    const demo = wrapper.find("[data-testid=workflow-demo]");
    await demo.find("input").setValue("rust");
    await demo.trigger("submit");
    await flushPromises();

    expect(mocked.launchSearch).toHaveBeenCalledWith("rust", "tok", "workflow");
    expect(router.currentRoute.value.name).toBe("search-detail");
    expect(router.currentRoute.value.params.id).toBe("j1");
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

  it("lists previous searches with their mode badge", async () => {
    mocked.listSearches.mockResolvedValue([
      { id: "j1", keyword: "one", mode: "workflow", status: "completed" },
      { id: "j2", keyword: "two", mode: "agent", status: "running" },
    ]);
    const { wrapper } = await mountView();

    const items = wrapper.findAll("li");
    expect(items).toHaveLength(2);
    expect(items[1].text()).toContain("two");
    expect(items[1].find(".mode").attributes("data-mode")).toBe("agent");
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
