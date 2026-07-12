import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "@/api";
import { useAuthStore } from "../auth";

// The api module is the store's only dependency; ApiError stays real.
vi.mock("@/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api")>();
  return {
    ...original,
    api: {
      login: vi.fn(),
      register: vi.fn(),
      logout: vi.fn(),
      refresh: vi.fn(),
    },
  };
});

const { api } = await import("@/api");
const mocked = api as unknown as {
  login: ReturnType<typeof vi.fn>;
  register: ReturnType<typeof vi.fn>;
  logout: ReturnType<typeof vi.fn>;
  refresh: ReturnType<typeof vi.fn>;
};

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
});

describe("auth store (ADR-008)", () => {
  it("login stores the access token in memory only", async () => {
    mocked.login.mockResolvedValue({ access_token: "tok-1" });
    const auth = useAuthStore();

    await auth.login("a@test.dev", "password");

    expect(auth.token).toBe("tok-1");
    expect(auth.isAuthenticated).toBe(true);
    expect(localStorage.length).toBe(0);
  });

  it("register creates the account then logs in", async () => {
    mocked.register.mockResolvedValue({ id: "u1", email: "a@test.dev" });
    mocked.login.mockResolvedValue({ access_token: "tok-2" });
    const auth = useAuthStore();

    await auth.register("a@test.dev", "password");

    expect(mocked.register).toHaveBeenCalledWith("a@test.dev", "password");
    expect(auth.token).toBe("tok-2");
  });

  it("logout clears the local session even when the server call fails", async () => {
    mocked.login.mockResolvedValue({ access_token: "tok" });
    mocked.logout.mockRejectedValue(new Error("network down"));
    const auth = useAuthStore();
    await auth.login("a@test.dev", "password");

    await auth.logout();

    expect(auth.isAuthenticated).toBe(false);
  });

  it("tryRefresh restores the session from the cookie, or clears it", async () => {
    const auth = useAuthStore();
    mocked.refresh.mockResolvedValue({ access_token: "restored" });
    expect(await auth.tryRefresh()).toBe(true);
    expect(auth.token).toBe("restored");

    mocked.refresh.mockRejectedValue(new ApiError(401, "expired"));
    expect(await auth.tryRefresh()).toBe(false);
    expect(auth.token).toBeNull();
  });

  it("withAuth rejects immediately without a session", async () => {
    const auth = useAuthStore();
    await expect(auth.withAuth(async () => "never")).rejects.toMatchObject({ status: 401 });
  });

  it("withAuth refreshes once and retries on 401", async () => {
    mocked.login.mockResolvedValue({ access_token: "stale" });
    mocked.refresh.mockResolvedValue({ access_token: "fresh" });
    const auth = useAuthStore();
    await auth.login("a@test.dev", "password");

    const call = vi
      .fn()
      .mockRejectedValueOnce(new ApiError(401, "expired"))
      .mockResolvedValueOnce("payload");

    await expect(auth.withAuth(call)).resolves.toBe("payload");
    expect(call).toHaveBeenNthCalledWith(1, "stale");
    expect(call).toHaveBeenNthCalledWith(2, "fresh");
  });

  it("withAuth clears the session when the refresh fails too", async () => {
    mocked.login.mockResolvedValue({ access_token: "stale" });
    mocked.refresh.mockRejectedValue(new ApiError(401, "gone"));
    const auth = useAuthStore();
    await auth.login("a@test.dev", "password");

    const call = vi.fn().mockRejectedValue(new ApiError(401, "expired"));

    await expect(auth.withAuth(call)).rejects.toMatchObject({ status: 401 });
    expect(auth.isAuthenticated).toBe(false);
  });

  it("withAuth passes non-401 errors through untouched", async () => {
    mocked.login.mockResolvedValue({ access_token: "tok" });
    const auth = useAuthStore();
    await auth.login("a@test.dev", "password");

    const failure = new ApiError(500, "boom");
    await expect(auth.withAuth(vi.fn().mockRejectedValue(failure))).rejects.toBe(failure);
    expect(mocked.refresh).not.toHaveBeenCalled();
    expect(auth.isAuthenticated).toBe(true); // a 500 is not a session problem
  });
});
