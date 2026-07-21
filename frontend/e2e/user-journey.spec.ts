// The real user journey through the browser (ADR-028), against the compose
// stack running with the deterministic fake providers (ADR-021):
// register -> launch a search -> live status -> timeline rendering (ADR-027).
import { expect, test } from "@playwright/test";

test("register, launch a search and read the results timeline", async ({ page }) => {
  const email = `e2e-${Date.now()}-${Math.floor(Math.random() * 1e6)}@test.dev`;

  await page.goto("/");
  await page.getByRole("button", { name: "No account yet? Sign up" }).click();
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill("e2e-s3cret-password");
  await page.getByRole("button", { name: "Sign up" }).click();

  // Registration logs the user in and lands on the searches view (two demo
  // blocks, ADR-030): run the workflow one.
  await expect(page.getByRole("heading", { name: "Workflow demo" })).toBeVisible();
  await page.getByTestId("workflow-demo").getByPlaceholder(/Keyword/).fill("playwright e2e");
  await page.getByRole("button", { name: "Run the workflow" }).click();

  // Detail view: live status (SSE with polling fallback, ADR-026) until the
  // fake-provider job completes.
  await expect(page.getByRole("heading", { name: "“playwright e2e”" })).toBeVisible();
  await expect(page.getByTestId("run-status")).toContainText("completed", { timeout: 30_000 });

  // Timeline (ADR-027): month groups in date order, newest first — one per
  // date-cascade stage (ADR-011/035).
  const months = page.locator(".timeline h3");
  await expect(months).toHaveText([
    "May 2026",
    "December 2025",
    "August 2025",
    "January 2023",
    "Unknown date",
  ]);

  // The LLM-dated hit is marked as estimated; every hit carries the fake
  // enrichment (event badge + summary).
  const estimated = page.locator(".entry.confidence-medium");
  await expect(estimated.getByText("(estimated)")).toBeVisible();
  await expect(estimated.getByRole("link")).toHaveText("fake-llm-datable");
  await expect(page.locator(".badge").first()).toHaveText("announcement");
  await expect(page.getByText("Fake summary for fake-dated-recent")).toBeVisible();

  // Undated results stay in their own section (ADR-011).
  const unknown = page.getByTestId("unknown-date-section");
  await expect(unknown.getByRole("link")).toHaveText("fake-undatable");
});

test("the agent demo streams the decision journal and renders the timeline", async ({ page }) => {
  const email = `e2e-${Date.now()}-${Math.floor(Math.random() * 1e6)}@test.dev`;

  await page.goto("/");
  await page.getByRole("button", { name: "No account yet? Sign up" }).click();
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill("e2e-s3cret-password");
  await page.getByRole("button", { name: "Sign up" }).click();

  await expect(page.getByRole("heading", { name: "Agent demo" })).toBeVisible();
  await page.getByTestId("agent-demo").getByPlaceholder(/Goal/).fill("agentic e2e");
  await page.getByRole("button", { name: "Run the agent" }).click();

  // The decision journal (ADR-030/031) fills in live: two searches (the
  // refined one deduplicated to 0 new results), a reasoned finish, then the
  // self-critique review.
  const journal = page.getByTestId("agent-journal");
  await expect(journal).toBeVisible({ timeout: 30_000 });
  await expect(journal.locator("li[data-kind]")).toHaveCount(4, { timeout: 30_000 });
  await expect(journal.locator("li[data-kind='search']").first()).toContainText("“agentic e2e”");
  await expect(journal.locator("li[data-kind='search']").nth(1)).toContainText("0 new results");
  await expect(journal.locator("li[data-kind='finish']")).toContainText("coverage looks sufficient");
  await expect(journal.locator("li[data-kind='critique']")).toContainText("reviewed the results");
  await expect(journal.locator("li[data-kind='critique']")).toContainText("All 5 results relate");

  // The loop's results land in the same timeline as the workflow mode.
  await expect(page.getByTestId("run-status")).toContainText("completed", { timeout: 30_000 });
  await expect(page.getByTestId("unknown-date-section")).toBeVisible();
});

test("the agent asks for clarification and resumes with the answer", async ({ page }) => {
  const email = `e2e-${Date.now()}-${Math.floor(Math.random() * 1e6)}@test.dev`;

  await page.goto("/");
  await page.getByRole("button", { name: "No account yet? Sign up" }).click();
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill("e2e-s3cret-password");
  await page.getByRole("button", { name: "Sign up" }).click();

  // The fake policy asks when the goal contains "ambiguous" (ADR-032).
  await expect(page.getByRole("heading", { name: "Agent demo" })).toBeVisible();
  await page.getByTestId("agent-demo").getByPlaceholder(/Goal/).fill("ambiguous e2e goal");
  await page.getByRole("button", { name: "Run the agent" }).click();

  const request = page.getByTestId("clarification-request");
  await expect(request).toBeVisible({ timeout: 30_000 });
  await expect(request).toContainText("Your goal looks ambiguous");
  // The awaiting_input status renders as a human sentence on the pill.
  await expect(page.getByTestId("run-status")).toContainText("needs your answer");

  await request.getByPlaceholder("Your answer").fill("the cars");
  await request.getByRole("button", { name: "Answer" }).click();

  // The dialog collapses into a recap and the resumed loop completes.
  await expect(page.getByTestId("clarification-recap")).toContainText("“the cars”", {
    timeout: 30_000,
  });
  await expect(page.getByTestId("run-status")).toContainText("completed", { timeout: 30_000 });
  const journal = page.getByTestId("agent-journal");
  await expect(journal.locator("li[data-kind]")).toHaveCount(4, { timeout: 30_000 });
  await expect(page.getByTestId("unknown-date-section")).toBeVisible();
});

test("recurring searches can be created and deleted (ADR-033)", async ({ page }) => {
  const email = `e2e-${Date.now()}-${Math.floor(Math.random() * 1e6)}@test.dev`;

  await page.goto("/");
  await page.getByRole("button", { name: "No account yet? Sign up" }).click();
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill("e2e-s3cret-password");
  await page.getByRole("button", { name: "Sign up" }).click();

  // Create a watch; the scheduled runs themselves are covered by the smoke
  // script (they need the scheduler tick, not a browser).
  const section = page.getByTestId("recurring-section");
  await expect(section).toBeVisible();
  await section.getByPlaceholder("Keyword to watch").fill("playwright watch");
  await section.getByRole("button", { name: "Watch" }).click();

  const item = section.locator("li", { hasText: "playwright watch" });
  await expect(item).toBeVisible();
  await expect(item).toContainText("every 60 min");

  await item.getByRole("button", { name: "Delete" }).click();
  await expect(item).not.toBeVisible();
  await expect(section.getByText("Nothing watched yet.")).toBeVisible();
});

test("a returning user logs in and finds the previous searches", async ({ page }) => {
  const email = `e2e-${Date.now()}-${Math.floor(Math.random() * 1e6)}@test.dev`;
  const password = "e2e-s3cret-password";

  await page.goto("/");
  await page.getByRole("button", { name: "No account yet? Sign up" }).click();
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign up" }).click();
  await expect(page.getByRole("heading", { name: "Workflow demo" })).toBeVisible();
  await page.getByTestId("workflow-demo").getByPlaceholder(/Keyword/).fill("history check");
  await page.getByRole("button", { name: "Run the workflow" }).click();
  await expect(page.getByTestId("run-status")).toContainText("completed", { timeout: 30_000 });

  // Fresh browser state = the HttpOnly refresh cookie is gone (ADR-008):
  // logging back in must list the search launched above.
  await page.context().clearCookies();
  await page.goto("/");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByRole("heading", { name: "Previous searches" })).toBeVisible();
  // Picking a past run loads it inline (ADR-039): same page, panel filled.
  await page.getByRole("button", { name: "history check" }).click();
  await expect(page.getByRole("heading", { name: "“history check”" })).toBeVisible();
  await expect(page.getByTestId("run-panel")).toContainText("completed");
});
