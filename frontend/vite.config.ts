/// <reference types="vitest/config" />
import { fileURLToPath, URL } from "node:url";

import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    // Same-origin API in development: the Rust backend runs on :8000.
    proxy: {
      "/api": "http://localhost:8000",
    },
  },
  test: {
    environment: "jsdom",
    // Playwright specs (e2e/) run against the compose stack, not under vitest.
    exclude: ["e2e/**", "node_modules/**", "dist/**"],
    coverage: {
      include: ["src/**"],
      // Thin wiring shell (app bootstrap): same policy as backend/src/main.rs
      // (ADR-023) — the logic lives in tested modules (router, stores, views).
      // Test helpers measure nothing either.
      exclude: ["src/main.ts", "src/**/__tests__/**"],
    },
  },
});
