import { describe, expect, it } from "vitest";

import { durationBetween, timeAgo } from "@/time";

const NOW = new Date("2026-07-21T12:00:00Z");

describe("timeAgo", () => {
  it.each([
    ["2026-07-21T11:59:40Z", "just now"],
    ["2026-07-21T11:58:00Z", "2 min ago"],
    ["2026-07-21T11:00:00Z", "1 h ago"],
    ["2026-07-21T04:30:00Z", "7 h ago"],
    ["2026-07-20T12:00:00Z", "1 d ago"],
    ["2026-07-15T12:00:00Z", "6 d ago"],
  ])("renders %s as %s", (iso, expected) => {
    expect(timeAgo(iso, NOW)).toBe(expected);
  });

  it("falls back to the calendar date beyond a week", () => {
    expect(timeAgo("2026-06-02T08:00:00Z", NOW)).toBe("2026-06-02");
  });

  it("never says the future — clock skew reads as just now", () => {
    expect(timeAgo("2026-07-21T12:00:30Z", NOW)).toBe("just now");
  });
});

describe("durationBetween", () => {
  it.each([
    ["2026-07-21T12:00:00Z", "2026-07-21T12:00:08Z", "8 s"],
    ["2026-07-21T12:00:00Z", "2026-07-21T12:02:30Z", "2 min 30 s"],
    ["2026-07-21T12:00:00Z", "2026-07-21T12:05:00Z", "5 min"],
    ["2026-07-21T12:00:00Z", "2026-07-21T13:10:00Z", "70 min"],
  ])("renders %s → %s as %s", (start, end, expected) => {
    expect(durationBetween(start, end)).toBe(expected);
  });
});
