// Thin typed client for the Rust backend (public API contracts, ARCHITECTURE.md §4).
//
// The zod schemas below are the single source of truth for the wire contract:
// the TS types are derived (`z.infer`), and responses are validated at runtime
// (`.parse`), so a backend drift surfaces as a clear client-side error instead
// of a silent `undefined`. The same shapes are pinned by `contracts/*.json` and
// asserted on the Rust side too (ADR-049). Objects are deliberately non-strict
// (zod strips unknown keys): an additive backend field is ignored, not
// rejected, so an older frontend keeps working during a rolling deploy.

import { z } from "zod";

export const dateConfidenceSchema = z.enum(["high", "medium", "unknown"]);
export type DateConfidence = z.infer<typeof dateConfidenceSchema>;

export const jobStatusSchema = z.enum([
  "pending",
  "running",
  "awaiting_input",
  "completed",
  "failed",
]);
export type JobStatus = z.infer<typeof jobStatusSchema>;

// Workflow = fixed pipeline; agent = LLM-driven decision loop (ADR-030).
export const jobModeSchema = z.enum(["workflow", "agent"]);
export type JobMode = z.infer<typeof jobModeSchema>;

export const eventTypeSchema = z.enum([
  "announcement",
  "release",
  "funding",
  "legal",
  "incident",
  "research",
  "opinion",
  "other",
]);
export type EventType = z.infer<typeof eventTypeSchema>;

export const searchResultSchema = z.object({
  title: z.string(),
  url: z.string(),
  snippet: z.string(),
  published_at: z.string().nullable(),
  date_confidence: dateConfidenceSchema,
  event_type: eventTypeSchema,
  summary: z.string().nullable(),
  // False when a previous run of a recurring search already saw it (ADR-033).
  is_new: z.boolean(),
});
export type SearchResult = z.infer<typeof searchResultSchema>;

// Accumulated API spend of a run (ADR-038).
export const jobUsageSchema = z.object({
  llm_calls: z.number(),
  llm_input_tokens: z.number(),
  llm_output_tokens: z.number(),
  search_calls: z.number(),
  cost_usd: z.number(),
});
export type JobUsage = z.infer<typeof jobUsageSchema>;

export const searchJobSchema = z.object({
  id: z.string(),
  keyword: z.string(),
  mode: jobModeSchema,
  status: jobStatusSchema,
  error: z.string().nullable(),
  // Clarification dialog (ADR-032): the agent's question and the user's answer.
  question: z.string().nullable(),
  answer: z.string().nullable(),
  // Set on scheduler-launched runs of a recurring search (ADR-033).
  recurring_search_id: z.string().nullable(),
  // Accumulated API spend (ADR-038).
  usage: jobUsageSchema,
  created_at: z.string(),
  completed_at: z.string().nullable(),
});
export type SearchJob = z.infer<typeof searchJobSchema>;

// A saved search re-run on an interval by the backend scheduler (ADR-033).
export const recurringSearchSchema = z.object({
  id: z.string(),
  keyword: z.string(),
  mode: jobModeSchema,
  interval_minutes: z.number(),
  // Digest target (ADR-036): notified when a run finds new results.
  webhook_url: z.string().nullable(),
  created_at: z.string(),
  last_run_at: z.string().nullable(),
});
export type RecurringSearch = z.infer<typeof recurringSearchSchema>;

// One decision of the agentic loop (ADR-030/031), shown in the live journal.
// `kind` stays an open string: newer agents may add step kinds before the frontend.
export const agentStepSchema = z.object({
  seq: z.number(),
  kind: z.string(),
  detail: z.string(),
  reason: z.string(),
  new_hits: z.number(),
});
export type AgentStep = z.infer<typeof agentStepSchema>;

export const searchJobDetailSchema = searchJobSchema.extend({
  results: z.array(searchResultSchema),
  steps: z.array(agentStepSchema),
});
export type SearchJobDetail = z.infer<typeof searchJobDetailSchema>;

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
      // Validate the streamed shape too (ADR-026/049): the SSE payload is the
      // same job detail as GET, so a drift is caught here as well.
      if (event.event === "update") onUpdate(searchJobDetailSchema.parse(JSON.parse(event.data)));
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

  // Data responses are validated against the wire schema (ADR-049): a backend
  // drift throws a ZodError here instead of surfacing as a silent `undefined`.
  listSearches: async (token: string) =>
    z.array(searchJobSchema).parse(await request<unknown>("/api/searches", {}, token)),

  // Recurring searches (ADR-033); the webhook receives digests (ADR-036).
  createRecurring: async (
    keyword: string,
    mode: JobMode,
    intervalMinutes: number,
    token: string,
    webhookUrl?: string,
  ) =>
    recurringSearchSchema.parse(
      await request<unknown>(
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
    ),

  listRecurring: async (token: string) =>
    z.array(recurringSearchSchema).parse(await request<unknown>("/api/recurring", {}, token)),

  deleteRecurring: (id: string, token: string) =>
    request<void>(`/api/recurring/${id}`, { method: "DELETE" }, token),

  // Answers the agent's clarification question (ADR-032): the job resumes.
  answerSearch: (id: string, answer: string, token: string) =>
    request<void>(
      `/api/searches/${id}/answer`,
      { method: "POST", body: JSON.stringify({ answer }) },
      token,
    ),

  getSearch: async (id: string, token: string) =>
    searchJobDetailSchema.parse(await request<unknown>(`/api/searches/${id}`, {}, token)),
};
