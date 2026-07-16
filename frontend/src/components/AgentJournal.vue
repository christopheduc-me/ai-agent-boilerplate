<script setup lang="ts">
// Live decision journal of the agentic loop (ADR-030/031): one entry per
// policy decision — searches, the finish, the self-critique review — streamed
// over SSE while the agent works. Rendering only — the reasons come verbatim
// from the agent.
import type { AgentStep } from "@/api";

defineProps<{ steps: AgentStep[]; live: boolean }>();

const icons: Record<string, string> = { finish: "✔", critique: "🧐", report: "📣" };
const icon = (kind: string): string => icons[kind] ?? "🔍";
</script>

<template>
  <section class="journal" data-testid="agent-journal">
    <h3>Agent decisions</h3>
    <ol>
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
      <li v-if="live" class="thinking" data-testid="agent-thinking">
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
  padding: 0.75rem 1rem;
  background: #f7f9fb;
  border: 1px solid #dde5ee;
  border-radius: 6px;
}
.journal h3 {
  margin: 0 0 0.5rem;
  font-size: 0.9rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: #666;
}
.journal ol {
  list-style: none;
  margin: 0;
  padding: 0;
}
.journal li {
  display: flex;
  gap: 0.6rem;
  padding: 0.3rem 0;
}
.icon {
  flex: none;
  width: 1.4rem;
  text-align: center;
}
.action {
  margin: 0;
}
.hits {
  color: #666;
  font-size: 0.85rem;
}
.reason {
  margin: 0;
  color: #666;
  font-size: 0.85rem;
  font-style: italic;
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
  color: #666;
}
</style>
