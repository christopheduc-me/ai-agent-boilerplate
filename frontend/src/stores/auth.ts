// Access token lives in memory only (ADR-008) — never in localStorage. The
// refresh token is an HttpOnly cookie managed entirely by the backend.
import { defineStore } from "pinia";
import { computed, ref } from "vue";

import { api, ApiError } from "@/api";

export const useAuthStore = defineStore("auth", () => {
  const token = ref<string | null>(null);
  const isAuthenticated = computed(() => token.value !== null);

  async function login(email: string, password: string): Promise<void> {
    const response = await api.login(email, password);
    token.value = response.access_token;
  }

  async function register(email: string, password: string): Promise<void> {
    await api.register(email, password);
    await login(email, password);
  }

  async function logout(): Promise<void> {
    token.value = null;
    await api.logout().catch(() => {}); // best effort: local state is already cleared
  }

  /** Silent session restore: trades the refresh cookie for a new access token.
   *  Returns whether a session is now active. Used on app start (the access
   *  token lives in memory and is lost on reload) and after a 401. */
  async function tryRefresh(): Promise<boolean> {
    try {
      const response = await api.refresh();
      token.value = response.access_token;
      return true;
    } catch {
      token.value = null;
      return false;
    }
  }

  /** Runs an authenticated API call; on 401, refreshes once and retries.
   *  Throws the original error when the session cannot be restored. */
  async function withAuth<T>(call: (token: string) => Promise<T>): Promise<T> {
    if (token.value === null) throw new ApiError(401, "not authenticated");
    try {
      return await call(token.value);
    } catch (error) {
      if (error instanceof ApiError && error.status === 401 && (await tryRefresh())) {
        return await call(token.value!);
      }
      if (error instanceof ApiError && error.status === 401) token.value = null;
      throw error;
    }
  }

  return { token, isAuthenticated, login, register, logout, tryRefresh, withAuth };
});
