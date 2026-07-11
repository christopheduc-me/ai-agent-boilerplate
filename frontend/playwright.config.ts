// Browser-level e2e tests (ADR-028). They drive the fully containerized
// compose stack (--profile full with AGENT_PROVIDERS=fake, ADR-021) through
// nginx — boot it first, then run `npm run test:e2e`. No webServer block on
// purpose: the stack under test is the real one, not a dev server.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://localhost:8080",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
