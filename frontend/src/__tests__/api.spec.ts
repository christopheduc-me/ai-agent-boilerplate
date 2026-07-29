import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError, createSSEParser } from "@/api";

describe("createSSEParser", () => {
  it("parses a complete event", () => {
    const parse = createSSEParser();
    expect(parse('event: update\ndata: {"status":"pending"}\n\n')).toEqual([
      { event: "update", data: '{"status":"pending"}' },
    ]);
  });

  it("reassembles events split across arbitrary chunk boundaries", () => {
    const parse = createSSEParser();
    expect(parse("event: upd")).toEqual([]);
    expect(parse('ate\ndata: {"a"')).toEqual([]);
    expect(parse(':1}\n\nevent: update\ndata: {"b":2}\n\n')).toEqual([
      { event: "update", data: '{"a":1}' },
      { event: "update", data: '{"b":2}' },
    ]);
  });

  it("ignores keep-alive comments and defaults the event name", () => {
    const parse = createSSEParser();
    expect(parse(":ping\n\ndata: hello\n\n")).toEqual([{ event: "message", data: "hello" }]);
  });
});

// ---------------------------------------------------------------- HTTP client

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

const jsonResponse = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

afterEach(() => fetchMock.mockReset());

describe("api client", () => {
  it("sends credentials and returns the parsed body", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, { access_token: "tok" }));

    await expect(api.login("a@test.dev", "pw")).resolves.toEqual({ access_token: "tok" });

    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("/api/auth/login");
    expect(options.method).toBe("POST");
    expect(JSON.parse(options.body)).toEqual({ email: "a@test.dev", password: "pw" });
  });

  it("turns error bodies into ApiError with the server message", async () => {
    fetchMock.mockResolvedValue(jsonResponse(429, { error: "quota reached" }));

    await expect(api.launchSearch("rust", "tok")).rejects.toMatchObject({
      status: 429,
      message: "quota reached",
    });
  });

  it("falls back to the status text when the error body is not JSON", async () => {
    fetchMock.mockResolvedValue(new Response("boom", { status: 502, statusText: "Bad Gateway" }));

    await expect(api.listSearches("tok")).rejects.toBeInstanceOf(ApiError);
  });

  it("attaches the bearer token on authenticated calls", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, []));

    await api.listSearches("tok-42");

    expect(fetchMock.mock.calls[0][1].headers["Authorization"]).toBe("Bearer tok-42");
  });

  it("launchSearch defaults to workflow and forwards the agent mode (ADR-030)", async () => {
    // A Response body is single-use: mint a fresh one per call.
    fetchMock.mockImplementation(() => Promise.resolve(jsonResponse(202, { job_id: "j1" })));

    await api.launchSearch("rust", "tok");
    await api.launchSearch("rust", "tok", "agent");

    expect(JSON.parse(fetchMock.mock.calls[0][1].body).mode).toBe("workflow");
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).mode).toBe("agent");
  });

  it("answerSearch posts the clarification answer (ADR-032)", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));

    await api.answerSearch("j1", "the car", "tok");

    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("/api/searches/j1/answer");
    expect(JSON.parse(options.body)).toEqual({ answer: "the car" });
  });

  it("returns undefined on 204 responses", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));

    await expect(api.logout()).resolves.toBeUndefined();
  });
});

describe("streamSearch (ADR-026)", () => {
  const sseBody = (chunks: string[]): ReadableStream<Uint8Array> => {
    const encoder = new TextEncoder();
    return new ReadableStream({
      start(controller) {
        for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
        controller.close();
      },
    });
  };

  it("emits one update per SSE event and resolves when the server closes", async () => {
    // A complete job detail: the stream is validated against the wire schema
    // (ADR-049), so a partial payload would (correctly) throw.
    const job = {
      id: "j1",
      keyword: "rust",
      mode: "agent",
      status: "completed",
      error: null,
      question: null,
      answer: null,
      recurring_search_id: null,
      usage: {
        llm_calls: 0,
        llm_input_tokens: 0,
        llm_output_tokens: 0,
        search_calls: 0,
        cost_usd: 0,
      },
      created_at: "2026-05-01T09:00:00Z",
      completed_at: "2026-05-01T09:00:12Z",
      results: [],
      steps: [],
    };
    fetchMock.mockResolvedValue(
      new Response(sseBody([`event: update\ndata: ${JSON.stringify(job)}\n\n`]), { status: 200 }),
    );
    const updates: unknown[] = [];

    await api.streamSearch("j1", "tok", (update) => updates.push(update));

    expect(updates).toEqual([job]);
    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("/api/searches/j1/events");
    expect(options.headers["Authorization"]).toBe("Bearer tok");
  });

  it("rejects with ApiError on a non-OK response so the caller falls back to polling", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 404, statusText: "Not Found" }));

    await expect(api.streamSearch("j1", "tok", () => {})).rejects.toMatchObject({ status: 404 });
  });
});
