<script setup lang="ts">
import { useRouter } from "vue-router";

import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

function logout(): void {
  auth.logout();
  router.push({ name: "login" });
}
</script>

<template>
  <header class="topbar">
    <div class="brand">
      <span class="brand-mark" aria-hidden="true" />
      <h1>AI Agent Boilerplate</h1>
    </div>
    <button v-if="auth.isAuthenticated" type="button" class="btn-ghost" @click="logout">
      Log out
    </button>
  </header>
  <main>
    <RouterView />
  </main>
</template>

<style>
:root {
  --bg: #f5f6f9;
  --surface: #ffffff;
  --surface-2: #f4f6fb;
  --border: #e4e8f0;
  --border-strong: #d2d8e4;
  --text: #1a2233;
  --text-muted: #667088;
  --accent: #7c3aed;
  --accent-hover: #6d28d9;
  --accent-soft: #f2ecfe;
  --accent-contrast: #ffffff;
  --danger: #dc2626;
  --success: #15803d;
  --success-bg: #ecfdf3;
  --success-border: #a6e9c2;
  --warn-bg: #fffaeb;
  --warn-border: #fbe08a;
  --warn-text: #b45309;
  --radius: 12px;
  --radius-sm: 8px;
  --shadow-sm: 0 1px 2px rgba(16, 24, 40, 0.06);
  --shadow: 0 6px 24px rgba(16, 24, 40, 0.08);
  --ring: 0 0 0 3px rgba(124, 58, 237, 0.22);
}

@media (prefers-color-scheme: dark) {
  :root {
    --bg: #0e1017;
    --surface: #171a23;
    --surface-2: #1e222e;
    --border: #2a2f3d;
    --border-strong: #363c4c;
    --text: #e7e9ef;
    --text-muted: #99a1b3;
    --accent: #8b5cf6;
    --accent-hover: #a78bfa;
    --accent-soft: #241d3a;
    --accent-contrast: #ffffff;
    --danger: #f87171;
    --success: #4ade80;
    --success-bg: rgba(34, 197, 94, 0.12);
    --success-border: rgba(34, 197, 94, 0.32);
    --warn-bg: rgba(251, 191, 36, 0.1);
    --warn-border: rgba(251, 191, 36, 0.32);
    --warn-text: #fbbf24;
    --shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.3);
    --shadow: 0 8px 28px rgba(0, 0, 0, 0.45);
    --ring: 0 0 0 3px rgba(139, 92, 246, 0.35);
  }
}

* {
  box-sizing: border-box;
}
/* Vestibular-safe: kill the pulse/shimmer loops and smooth scrolling. */
@media (prefers-reduced-motion: reduce) {
  *,
  *::before,
  *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
    scroll-behavior: auto !important;
  }
}
html {
  color-scheme: light dark;
}
body {
  margin: 0;
  font-family:
    system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  background: var(--bg);
  color: var(--text);
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
}
h1,
h2,
h3 {
  line-height: 1.25;
}
a {
  color: var(--accent);
  text-decoration: none;
}
a:hover {
  text-decoration: underline;
}

/* --- form controls --- */
input,
select {
  width: 100%;
  padding: 0.5rem 0.65rem;
  font: inherit;
  color: var(--text);
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  transition:
    border-color 0.12s ease,
    box-shadow 0.12s ease;
}
input::placeholder {
  color: var(--text-muted);
}
input:focus,
select:focus,
button:focus-visible {
  outline: none;
  border-color: var(--accent);
  box-shadow: var(--ring);
}

/* Default button = primary action */
button {
  font: inherit;
  font-weight: 600;
  color: var(--accent-contrast);
  background: var(--accent);
  border: 1px solid transparent;
  border-radius: var(--radius-sm);
  padding: 0.5rem 0.9rem;
  cursor: pointer;
  transition:
    background 0.12s ease,
    opacity 0.12s ease,
    transform 0.06s ease;
}
button:hover {
  background: var(--accent-hover);
}
button:active {
  transform: translateY(1px);
}
button:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}
.btn-ghost {
  color: var(--text);
  background: transparent;
  border: 1px solid var(--border-strong);
}
.btn-ghost:hover {
  background: var(--surface-2);
}

main {
  max-width: 1500px;
  margin: 0 auto;
  padding: 1.25rem 1.5rem 1.5rem;
}
/* Slim, theme-aware scrollbars for the panes that scroll on their own. */
.scroll-pane {
  scrollbar-width: thin;
  scrollbar-color: var(--border-strong) transparent;
}
.scroll-pane::-webkit-scrollbar {
  width: 10px;
  height: 10px;
}
.scroll-pane::-webkit-scrollbar-thumb {
  background: var(--border-strong);
  border-radius: 999px;
  border: 2px solid transparent;
  background-clip: padding-box;
}
.scroll-pane::-webkit-scrollbar-thumb:hover {
  background: var(--text-muted);
  background-clip: padding-box;
}
.topbar {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 0.7rem 1.25rem;
  background: color-mix(in srgb, var(--surface) 85%, transparent);
  backdrop-filter: saturate(1.4) blur(8px);
  border-bottom: 1px solid var(--border);
}
.brand {
  display: flex;
  align-items: center;
  gap: 0.6rem;
}
.brand-mark {
  width: 20px;
  height: 20px;
  border-radius: 6px;
  background: linear-gradient(135deg, #8b5cf6, #6d28d9);
  box-shadow: var(--shadow-sm);
}
.topbar h1 {
  font-size: 1.05rem;
  font-weight: 700;
  margin: 0;
  letter-spacing: -0.01em;
}
.error {
  color: var(--danger);
}
</style>
