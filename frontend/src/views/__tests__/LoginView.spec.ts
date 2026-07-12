import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import LoginView from "../LoginView.vue";
import { makePinia, makeRouter } from "./helpers";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return { ...original, api: { login: vi.fn(), register: vi.fn() } };
});

const { api, ApiError } = await import("@/api");
const mocked = api as unknown as Record<"login" | "register", ReturnType<typeof vi.fn>>;

async function mountView() {
  const router = makeRouter();
  await router.push({ name: "login" });
  const wrapper = mount(LoginView, { global: { plugins: [makePinia(), router] } });
  return { wrapper, router };
}

beforeEach(() => vi.clearAllMocks());

describe("LoginView", () => {
  it("logs in and navigates to the searches view", async () => {
    mocked.login.mockResolvedValue({ access_token: "tok" });
    const { wrapper, router } = await mountView();

    await wrapper.find("input[type=email]").setValue("a@test.dev");
    await wrapper.find("input[type=password]").setValue("s3cret-password");
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(mocked.login).toHaveBeenCalledWith("a@test.dev", "s3cret-password");
    expect(router.currentRoute.value.name).toBe("searches");
  });

  it("toggles to sign-up mode and registers", async () => {
    mocked.register.mockResolvedValue({ id: "u1", email: "a@test.dev" });
    mocked.login.mockResolvedValue({ access_token: "tok" });
    const { wrapper } = await mountView();

    await wrapper.find("button.link").trigger("click");
    expect(wrapper.find("h2").text()).toBe("Create an account");

    await wrapper.find("input[type=email]").setValue("a@test.dev");
    await wrapper.find("input[type=password]").setValue("s3cret-password");
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(mocked.register).toHaveBeenCalledWith("a@test.dev", "s3cret-password");
  });

  it("surfaces API errors and stays on the page", async () => {
    mocked.login.mockRejectedValue(new ApiError(401, "invalid credentials"));
    const { wrapper, router } = await mountView();

    await wrapper.find("input[type=email]").setValue("a@test.dev");
    await wrapper.find("input[type=password]").setValue("wrong-password");
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(wrapper.find(".error").text()).toBe("invalid credentials");
    expect(router.currentRoute.value.name).toBe("login");
  });
});
