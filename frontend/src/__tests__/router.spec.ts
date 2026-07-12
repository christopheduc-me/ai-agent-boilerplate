// The auth guard (ADR-008): unauthenticated navigation attempts a silent
// refresh from the HttpOnly cookie before falling back to the login page.
import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { router } from "@/router";
import { useAuthStore } from "@/stores/auth";

vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return { ...original, api: { refresh: vi.fn(), listSearches: vi.fn().mockResolvedValue([]) } };
});

const { api } = await import("@/api");
const mockedRefresh = (api as unknown as { refresh: ReturnType<typeof vi.fn> }).refresh;

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
});

describe("router auth guard", () => {
  it("redirects to login when no session can be restored", async () => {
    mockedRefresh.mockRejectedValue(new Error("no cookie"));

    await router.push({ name: "searches" });

    expect(router.currentRoute.value.name).toBe("login");
  });

  it("restores the session silently and lets the navigation through", async () => {
    mockedRefresh.mockResolvedValue({ access_token: "restored" });

    await router.push({ name: "searches" });

    expect(router.currentRoute.value.name).toBe("searches");
    expect(useAuthStore().token).toBe("restored");
  });

  it("never guards the login page itself", async () => {
    await router.push({ name: "login" });

    expect(router.currentRoute.value.name).toBe("login");
    expect(mockedRefresh).not.toHaveBeenCalled();
  });
});
