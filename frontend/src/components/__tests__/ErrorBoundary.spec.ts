import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick } from "vue";

import ErrorBoundary from "../ErrorBoundary.vue";

// A child that throws during render — the failure mode ADR-073 exists for.
// API errors are handled in the views themselves; this is the kind Vue answers
// by unmounting the tree.
const Exploding = defineComponent({
  setup() {
    return () => {
      throw new Error("render blew up");
    };
  },
});

const Fine = defineComponent({
  setup: () => () => h("p", { "data-testid": "child" }, "all good"),
});

const push = vi.fn();
vi.mock("vue-router", () => ({ useRouter: () => ({ push }) }));

describe("ErrorBoundary", () => {
  beforeEach(() => {
    push.mockClear();
    // Vue re-throws captured errors to the console; keep the suite output clean
    // while still asserting we logged.
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders its slot untouched while nothing fails", () => {
    const wrapper = mount(ErrorBoundary, { slots: { default: () => h(Fine) } });

    expect(wrapper.find('[data-testid="child"]').exists()).toBe(true);
    expect(wrapper.find('[data-testid="error-boundary"]').exists()).toBe(false);
  });

  it("shows the fallback instead of a blank page when a child throws", async () => {
    const wrapper = mount(ErrorBoundary, { slots: { default: () => h(Exploding) } });
    await nextTick();

    const fallback = wrapper.find('[data-testid="error-boundary"]');
    expect(fallback.exists()).toBe(true);
    // role=alert so a screen reader announces it, not just sighted users.
    expect(fallback.attributes("role")).toBe("alert");
    expect(wrapper.text()).toContain("Something went wrong");
  });

  it("logs the error with Vue's hook name, so it is diagnosable", () => {
    mount(ErrorBoundary, { slots: { default: () => h(Exploding) } });

    expect(console.error).toHaveBeenCalled();
    const [label, error] = vi.mocked(console.error).mock.calls[0];
    expect(String(label)).toContain("[ErrorBoundary]");
    expect((error as Error).message).toBe("render blew up");
  });

  it("recovers on retry without reloading, keeping the session", async () => {
    let shouldThrow = true;
    const Flaky = defineComponent({
      setup() {
        return () => {
          if (shouldThrow) throw new Error("transient");
          return h("p", { "data-testid": "child" }, "recovered");
        };
      },
    });

    const wrapper = mount(ErrorBoundary, { slots: { default: () => h(Flaky) } });
    await nextTick();
    expect(wrapper.find('[data-testid="error-boundary"]').exists()).toBe(true);

    shouldThrow = false;
    await wrapper.find('[data-testid="error-boundary-retry"]').trigger("click");

    expect(wrapper.find('[data-testid="error-boundary"]').exists()).toBe(false);
    expect(wrapper.find('[data-testid="child"]').text()).toBe("recovered");
  });

  it("offers a way out that does not depend on the broken view", async () => {
    const wrapper = mount(ErrorBoundary, { slots: { default: () => h(Exploding) } });
    await nextTick();

    await wrapper.find('[data-testid="error-boundary-home"]').trigger("click");

    expect(push).toHaveBeenCalledWith({ name: "home" });
  });
});
