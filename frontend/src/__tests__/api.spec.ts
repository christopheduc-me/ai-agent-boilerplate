import { describe, expect, it } from "vitest";

import { createSSEParser } from "@/api";

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
