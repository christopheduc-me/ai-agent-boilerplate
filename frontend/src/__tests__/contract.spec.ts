// Cross-language contract fixtures (ADR-025/049): the frontend consumer side.
//
// The backend PRODUCES the public wire shapes (asserted byte-for-byte in
// backend/tests/contract.rs); here the frontend CONSUMES the same fixtures,
// validating them against the zod schemas that back `api.ts`. A drift in the
// Rust serialization breaks one of the two suites instead of reaching a user.

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { recurringSearchSchema, searchJobDetailSchema } from "../api";

// Resolve from this file (…/frontend/src/__tests__) up to the repo-root
// contracts/. Plain path math — not `new URL(literal, import.meta.url)`, which
// Vite would try to bundle as an asset.
const here = dirname(fileURLToPath(import.meta.url));
const load = (name: string): unknown =>
  JSON.parse(readFileSync(resolve(here, "../../../contracts", name), "utf-8"));

describe("public API contract fixtures (ADR-049)", () => {
  it("search-job-detail.json validates against the job detail schema", () => {
    const parsed = searchJobDetailSchema.parse(load("search-job-detail.json"));
    // A representative field of each nested shape is present and typed.
    expect(parsed.usage.cost_usd).toBeTypeOf("number");
    expect(parsed.results[0].event_type).toBe("release");
    expect(parsed.results[1].published_at).toBeNull();
    expect(parsed.steps.map((s) => s.kind)).toEqual(["search", "finish"]);
  });

  it("recurring-search.json validates against the recurring schema", () => {
    const parsed = recurringSearchSchema.parse(load("recurring-search.json"));
    expect(parsed.interval_minutes).toBe(1440);
    expect(parsed.webhook_url).toBeTypeOf("string");
  });

  it("tolerates unknown backend fields (forward-compatible rolling deploys)", () => {
    // An older frontend must keep working when a newer backend adds a field:
    // zod strips unknown keys rather than rejecting (ADR-049 back-compat rule).
    const withExtra = { ...(load("recurring-search.json") as object), brand_new_field: 42 };
    const parsed = recurringSearchSchema.parse(withExtra);
    expect("brand_new_field" in parsed).toBe(false);
  });
});
