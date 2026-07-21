<script setup lang="ts">
// One pill per job status, colored consistently everywhere a status shows
// (run panel header, history rows). Rendering only.
import type { JobStatus } from "@/api";

defineProps<{ status: JobStatus }>();
</script>

<template>
  <span class="status" :data-status="status">{{
    status === "awaiting_input" ? "needs your answer" : status
  }}</span>
</template>

<style scoped>
.status {
  font-weight: 700;
  font-size: 0.78rem;
  text-transform: capitalize;
  padding: 0.1rem 0.55rem;
  border-radius: 999px;
  background: var(--surface-2);
  border: 1px solid var(--border-strong);
  color: var(--text);
  white-space: nowrap;
}
.status[data-status="completed"] {
  background: var(--success-bg);
  border-color: var(--success-border);
  color: var(--success);
}
.status[data-status="failed"] {
  background: color-mix(in srgb, var(--danger) 12%, transparent);
  border-color: color-mix(in srgb, var(--danger) 35%, transparent);
  color: var(--danger);
}
.status[data-status="awaiting_input"] {
  text-transform: none; /* a sentence, not a status keyword */
  background: var(--warn-bg);
  border-color: var(--warn-border);
  color: var(--warn-text);
}
</style>
