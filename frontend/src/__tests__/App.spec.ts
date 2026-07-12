import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { describe, expect, it, vi } from "vitest";
import { createMemoryHistory, createRouter } from "vue-router";

import App from "@/App.vue";
import { useAuthStore } from "@/stores/auth";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return { ...original, api: { logout: vi.fn().mockResolvedValue(undefined) } };
});

describe("App shell", () => {
  it("shows the logout button only when authenticated, and logs out", async () => {
    const pinia = createPinia();
    setActivePinia(pinia);
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: "/", name: "searches", component: { template: "<div />" } },
        { path: "/login", name: "login", component: { template: "<div />" } },
      ],
    });
    const wrapper = mount(App, { global: { plugins: [pinia, router] } });
    expect(wrapper.find("button").exists()).toBe(false);

    useAuthStore().token = "tok";
    await flushPromises();
    await wrapper.vm.$nextTick();
    await wrapper.find("button").trigger("click");
    await flushPromises();

    expect(useAuthStore().isAuthenticated).toBe(false);
    expect(router.currentRoute.value.name).toBe("login");
  });
});
