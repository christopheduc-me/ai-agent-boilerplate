// Thin typed client for the Rust backend (public API contracts, ARCHITECTURE.md §4).

export type DateConfidence = "high" | "medium" | "unknown";
export type JobStatus = "pending" | "running" | "awaiting_input" | "completed" | "failed";
// Workflow = fixed pipeline; agent = LLM-driven decision loop (ADR-030).
export type JobMode = "workflow" | "agent";
export type EventType =
  | "announcement"
  | "release"
  | "funding"
  | "legal"
  | "incident"
  | "research"
  | "opinion"
  | "other";

export interface SearchResult {
  title: string;
  url: string;
  snippet: string;
  published_at: string | null;
  date_confidence: DateConfidence;
  event_type: EventType;
  summary: string | null;
  // False when a previous run of a recurring search already saw it (ADR-033).
  is_new: boolean;
}

export interface SearchJob {
  id: string;
  keyword: string;
  mode: JobMode;
  status: JobStatus;
  error: string | null;
  // Clarification dialog (ADR-032): the agent's question and the user's answer.
  question: string | null;
  answer: string | null;
  // Set on scheduler-launched runs of a recurring search (ADR-033).
  recurring_search_id: string | null;
  created_at: string;
  completed_at: string | null;
}

// A saved search re-run on an interval by the backend scheduler (ADR-033).
export interface RecurringSearch {
  id: string;
  keyword: string;
  mode: JobMode;
  interval_minutes: number;
  // Digest target (ADR-036): notified when a run finds new results.
  webhook_url: string | null;
  created_at: string;
  last_run_at: string | null;
}

// One decision of the agentic loop (ADR-030/031), shown in the live journal.
// The kind stays open: newer agents may add step kinds before the frontend.
export interface AgentStep {
  seq: number;
  kind: "search" | "finish" | "critique" | string;
  detail: string;
  reason: string;
  new_hits: number;
}

export interface SearchJobDetail extends SearchJob {
  results: SearchResult[];
  steps: AgentStep[];
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

  launchSearch: (keyword: string, token: string, mode: JobMode = "workflow") =>
    request<{ job_id: string }>(
      "/api/searches",
      { method: "POST", body: JSON.stringify({ keyword, mode }) },
      token,
    ),

  listSearches: (token: string) => request<SearchJob[]>("/api/searches", {}, token),

  // Recurring searches (ADR-033); the webhook receives digests (ADR-036).
  createRecurring: (
    keyword: string,
    mode: JobMode,
    intervalMinutes: number,
    token: string,
    webhookUrl?: string,
  ) =>
    request<RecurringSearch>(
      "/api/recurring",
      {
        method: "POST",
        body: JSON.stringify({
          keyword,
          mode,
          interval_minutes: intervalMinutes,
          webhook_url: webhookUrl || null,
        }),
      },
      token,
    ),

  listRecurring: (token: string) => request<RecurringSearch[]>("/api/recurring", {}, token),

  deleteRecurring: (id: string, token: string) =>
    request<void>(`/api/recurring/${id}`, { method: "DELETE" }, token),

  // Answers the agent's clarification question (ADR-032): the job resumes.
  answerSearch: (id: string, answer: string, token: string) =>
    request<void>(
      `/api/searches/${id}/answer`,
      { method: "POST", body: JSON.stringify({ answer }) },
      token,
    ),

  getSearch: (id: string, token: string) =>
    request<SearchJobDetail>(`/api/searches/${id}`, {}, token),
};
