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

export interface SSEEvent {
  event: string;
  data: string;
}

/**
 * Incremental parser for a text/event-stream body (ADR-026). Feed it chunks
 * (arbitrarily split), it returns the complete events found so far.
 */
export function createSSEParser(): (chunk: string) => SSEEvent[] {
  let buffer = "";
  return (chunk: string): SSEEvent[] => {
    buffer += chunk;
    const events: SSEEvent[] = [];
    let separator: number;
    while ((separator = buffer.indexOf("\n\n")) !== -1) {
      const block = buffer.slice(0, separator);
      buffer = buffer.slice(separator + 2);
      let event = "message";
      const data: string[] = [];
      for (const line of block.split("\n")) {
        if (line.startsWith("event:")) event = line.slice(6).trim();
        else if (line.startsWith("data:")) data.push(line.slice(5).trim());
        // lines starting with ":" are keep-alive comments — ignored
      }
      if (data.length > 0) events.push({ event, data: data.join("\n") });
    }
    return events;
  };
}

/**
 * Streams job updates over SSE. `EventSource` cannot send an Authorization
 * header, so this uses fetch + ReadableStream instead (ADR-026). Resolves when
 * the server closes the stream (terminal status); rejects on transport errors
 * so the caller can fall back to polling.
 */
async function streamSearch(
  id: string,
  token: string,
  onUpdate: (job: SearchJobDetail) => void,
  signal?: AbortSignal,
): Promise<void> {
  const response = await fetch(`/api/searches/${id}/events`, {
    headers: { Accept: "text/event-stream", Authorization: `Bearer ${token}` },
    signal,
  });
  if (!response.ok || !response.body) {
    throw new ApiError(response.status, response.statusText);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const parse = createSSEParser();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return;
    for (const event of parse(decoder.decode(value, { stream: true }))) {
      if (event.event === "update") onUpdate(JSON.parse(event.data) as SearchJobDetail);
    }
  }
}

export const api = {
  streamSearch,
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
