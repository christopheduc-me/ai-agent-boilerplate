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
  await expect(page.getByText("Status:")).toContainText("completed", { timeout: 30_000 });

  // Timeline (ADR-027): month groups in date order, newest first.
  const months = page.locator(".timeline h3");
  await expect(months).toHaveText(["May 2026", "August 2025", "January 2023", "Unknown date"]);

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
  await expect(journal.locator("li[data-kind='critique']")).toContainText("All 4 results relate");

  // The loop's results land in the same timeline as the workflow mode.
  await expect(page.getByText("Status:")).toContainText("completed", { timeout: 30_000 });
  await expect(page.getByTestId("unknown-date-section")).toBeVisible();
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
  await expect(page.getByText("Status:")).toContainText("completed", { timeout: 30_000 });

  // Fresh browser state = the HttpOnly refresh cookie is gone (ADR-008):
  // logging back in must list the search launched above.
  await page.context().clearCookies();
  await page.goto("/");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByRole("heading", { name: "Previous searches" })).toBeVisible();
  await expect(page.getByRole("link", { name: "history check" })).toBeVisible();
});
