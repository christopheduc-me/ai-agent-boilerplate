// Thin typed client for the Rust backend (public API contracts, ARCHITECTURE.md §4).

export type DateConfidence = "high" | "medium" | "unknown";
export type JobStatus = "pending" | "running" | "completed" | "failed";

export interface SearchResult {
  title: string;
  url: string;
  snippet: string;
  published_at: string | null;
  date_confidence: DateConfidence;
}

export interface SearchJob {
  id: string;
  keyword: string;
  status: JobStatus;
  error: string | null;
  created_at: string;
  completed_at: string | null;
}

export interface SearchJobDetail extends SearchJob {
  results: SearchResult[];
}

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(path: string, options: RequestInit = {}, token?: string): Promise<T> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers["Authorization"] = `Bearer ${token}`;
  const response = await fetch(path, { ...options, headers });
  if (!response.ok) {
    const body = (await response.json().catch(() => ({}))) as { error?: string };
    throw new ApiError(response.status, body.error ?? response.statusText);
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

export const api = {
  register: (email: string, password: string) =>
    request<{ id: string; email: string }>("/api/auth/register", {
      method: "POST",
      body: JSON.stringify({ email, password }),
    }),

  login: (email: string, password: string) =>
    request<{ access_token: string }>("/api/auth/login", {
      method: "POST",
      body: JSON.stringify({ email, password }),
    }),

  // The refresh token travels in an HttpOnly cookie (ADR-008) — the browser
  // attaches it automatically on same-origin requests.
  refresh: () => request<{ access_token: string }>("/api/auth/refresh", { method: "POST" }),

  logout: () => request<void>("/api/auth/logout", { method: "POST" }),

  launchSearch: (keyword: string, token: string) =>
    request<{ job_id: string }>(
      "/api/searches",
      { method: "POST", body: JSON.stringify({ keyword }) },
      token,
    ),

  listSearches: (token: string) => request<SearchJob[]>("/api/searches", {}, token),

  getSearch: (id: string, token: string) =>
    request<SearchJobDetail>(`/api/searches/${id}`, {}, token),
};
