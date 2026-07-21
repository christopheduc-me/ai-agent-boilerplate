<script setup lang="ts">
// Live decision journal of the agentic loop (ADR-030/031): one entry per
// policy decision — searches, the finish, the self-critique review — streamed
// over SSE while the agent works. Rendering only — the reasons come verbatim
// from the agent.
import { ref, watch } from "vue";

import type { AgentStep } from "@/api";

const props = defineProps<{ steps: AgentStep[]; live: boolean }>();

const icons: Record<string, string> = { finish: "✔", critique: "🧐", report: "📣" };
const icon = (kind: string): string => icons[kind] ?? "🔍";

// Auto-follow: while the run is live the journal grows inside a bounded
// scroll pane (ADR-039) — keep the newest entry in view so the user watches
// the loop think without scrolling. Finished runs are left alone.
const tail = ref<HTMLElement | null>(null);
watch(
  () => props.steps.length,
  async () => {
    if (!props.live) return;
    await new Promise(requestAnimationFrame); // let the new entry render
    tail.value?.scrollIntoView?.({ block: "nearest", behavior: "smooth" });
  },
);
</script>

<template>
  <section class="journal" data-testid="agent-journal">
    <h3>Agent decisions</h3>
    <ol aria-live="polite">
      <li v-for="step in steps" :key="step.seq" :data-kind="step.kind">
        <span class="icon" aria-hidden="true">{{ icon(step.kind) }}</span>
        <div>
          <p class="action">
            <template v-if="step.kind === 'search'">
              searched <strong>“{{ step.detail }}”</strong>
              <span class="hits">→ {{ step.new_hits }} new result{{ step.new_hits === 1 ? "" : "s" }}</span>
            </template>
            <template v-else-if="step.kind === 'critique'">reviewed the results</template>
            <template v-else-if="step.kind === 'report'">compared with the previous run</template>
            <template v-else>finished</template>
          </p>
          <p class="reason">{{ step.reason }}</p>
        </div>
      </li>
      <li v-if="live" ref="tail" class="thinking" data-testid="agent-thinking">
        <span class="icon pulse" aria-hidden="true">…</span>
        <p class="action">thinking</p>
      </li>
    </ol>
    <p v-if="steps.length === 0 && !live" class="empty">No decisions recorded.</p>
  </section>
</template>

<style scoped>
.journal {
  margin: 1rem 0;
  padding: 1rem 1.1rem;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
.journal h3 {
  margin: 0 0 0.75rem;
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
}
.journal ol {
  list-style: none;
  margin: 0;
  padding: 0;
}
.journal li {
  display: flex;
  gap: 0.7rem;
  padding: 0.45rem 0;
}
.journal li + li {
  border-top: 1px dashed var(--border);
}
.icon {
  flex: none;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 1.7rem;
  height: 1.7rem;
  font-size: 0.85rem;
  border-radius: 50%;
  background: var(--surface);
  border: 1px solid var(--border-strong);
}
.action {
  margin: 0;
  font-size: 0.92rem;
}
.hits {
  color: var(--text-muted);
  font-size: 0.85rem;
}
.reason {
  margin: 0.1rem 0 0;
  color: var(--text-muted);
  font-size: 0.85rem;
  font-style: italic;
}
.thinking .icon {
  border-color: var(--accent);
  color: var(--accent);
}
.pulse {
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  50% {
    opacity: 0.25;
  }
}
.empty {
  margin: 0;
  color: var(--text-muted);
}
</style>
