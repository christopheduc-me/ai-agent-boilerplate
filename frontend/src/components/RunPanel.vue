<script setup lang="ts">
// Inline run panel (ADR-039): follows one job live — SSE with polling
// fallback (ADR-026), the agent journal (ADR-030), the clarification dialog
// (ADR-032), the cost line (ADR-038) and the results timeline — all in place,
// no navigation. The `id` prop can change (the user picks another run from
// the history): the previous stream is torn down and a new one starts.
import { onBeforeUnmount, ref, watch } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type SearchJobDetail } from "@/api";
import AgentJournal from "@/components/AgentJournal.vue";
import ResultTimeline from "@/components/ResultTimeline.vue";
import { useAuthStore } from "@/stores/auth";

const props = defineProps<{ id: string }>();
const emit = defineEmits<{ finished: [] }>();
const auth = useAuthStore();
const router = useRouter();

const job = ref<SearchJobDetail | null>(null);
const answer = ref("");
const answerError = ref<string | null>(null);
const answering = ref(false);
let timer: ReturnType<typeof setInterval> | undefined;
let abort = new AbortController();
let finishedNotified = false;

function isTerminal(): boolean {
  return job.value?.status === "completed" || job.value?.status === "failed";
}

function onUpdate(update: SearchJobDetail): void {
  job.value = update;
  if (isTerminal()) {
    clearInterval(timer);
    if (!finishedNotified) {
      finishedNotified = true;
      emit("finished"); // let the workbench refresh the history/costs
    }
  }
}

async function refresh(): Promise<void> {
  try {
    onUpdate(await auth.withAuth((token) => api.getSearch(props.id, token)));
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      clearInterval(timer);
      router.push({ name: "login" });
    }
  }
}

function startPolling(): void {
  refresh();
  timer = setInterval(refresh, 2500);
}

/** Sends the clarification answer (ADR-032); the live updates carry the
 *  resumed run through to completion. */
async function submitAnswer(): Promise<void> {
  answerError.value = null;
  answering.value = true;
  try {
    await auth.withAuth((token) => api.answerSearch(props.id, answer.value, token));
    answer.value = "";
    await refresh();
  } catch (e) {
    answerError.value = e instanceof ApiError ? e.message : "unexpected error";
  } finally {
    answering.value = false;
  }
}

async function follow(): Promise<void> {
  // Live updates over SSE (ADR-026); any failure falls back to polling.
  const signal = abort.signal;
  try {
    await auth.withAuth((token) => api.streamSearch(props.id, token, onUpdate, signal));
    if (!signal.aborted && !isTerminal()) startPolling(); // stream ended early
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      router.push({ name: "login" });
      return;
    }
    if (!signal.aborted) startPolling();
  }
}

function stop(): void {
  abort.abort();
  clearInterval(timer);
}

watch(
  () => props.id,
  () => {
    stop();
    abort = new AbortController();
    job.value = null;
    answer.value = "";
    answerError.value = null;
    finishedNotified = false;
    follow();
  },
  { immediate: true },
);
onBeforeUnmount(stop);
</script>

<template>
  <section v-if="job" class="run" data-testid="run-panel">
    <h2>“{{ job.keyword }}”</h2>
    <p class="status-line">
      Status:
      <span class="status" :data-status="job.status">{{ job.status }}</span>
      <span v-if="job.status === 'pending' || job.status === 'running'" class="live">
        <span class="dot" aria-hidden="true" /> live
      </span>
    </p>
    <p v-if="job.error" class="error">{{ job.error }}</p>
    <!-- API spend of this run (ADR-038); $0 with the fake providers. -->
    <p
      v-if="job.usage.llm_calls > 0 || job.usage.search_calls > 0"
      class="cost"
      data-testid="job-cost"
    >
      💸 <strong>${{ job.usage.cost_usd.toFixed(4) }}</strong> — {{ job.usage.llm_calls }} LLM
      call{{ job.usage.llm_calls === 1 ? "" : "s" }} ({{ job.usage.llm_input_tokens }} in /
      {{ job.usage.llm_output_tokens }} out tokens), {{ job.usage.search_calls }} search{{
        job.usage.search_calls === 1 ? "" : "es"
      }}
    </p>

    <!-- HITL (ADR-032): the agent asked a question; the job waits for you. -->
    <section
      v-if="job.status === 'awaiting_input' && job.question"
      class="clarification"
      data-testid="clarification-request"
    >
      <p class="question">🙋 The agent asks: <strong>{{ job.question }}</strong></p>
      <form @submit.prevent="submitAnswer">
        <input v-model="answer" placeholder="Your answer" required :disabled="answering" />
        <button type="submit" :disabled="answering">Answer</button>
      </form>
      <p v-if="answerError" class="error">{{ answerError }}</p>
    </section>
    <!-- After the answer: keep the dialog visible as context. -->
    <p v-else-if="job.question && job.answer" class="clarified" data-testid="clarification-recap">
      🙋 {{ job.question }} — <strong>“{{ job.answer }}”</strong>
    </p>

    <!-- Agent mode (ADR-030): the decision journal streams in live over SSE. -->
    <AgentJournal
      v-if="job.mode === 'agent'"
      :steps="job.steps"
      :live="job.status === 'pending' || job.status === 'running'"
    />
    <ResultTimeline
      v-if="job.status === 'completed'"
      :results="job.results"
      :highlight-new="job.recurring_search_id !== null"
    />
  </section>
  <p v-else class="loading">Loading…</p>
</template>

<style scoped>
.run h2 {
  margin: 0 0 0.75rem;
  font-size: 1.35rem;
  letter-spacing: -0.02em;
}
.status-line {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin: 0 0 0.75rem;
  color: var(--text-muted);
  font-size: 0.9rem;
}
.status {
  font-weight: 700;
  text-transform: capitalize;
  padding: 0.1rem 0.55rem;
  border-radius: 999px;
  background: var(--surface-2);
  border: 1px solid var(--border-strong);
  color: var(--text);
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
  background: var(--warn-bg);
  border-color: var(--warn-border);
  color: #b45309;
}
.live {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  color: var(--accent);
  font-weight: 600;
}
.live .dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent);
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  50% {
    opacity: 0.25;
  }
}
.clarification {
  margin: 1rem 0;
  padding: 0.9rem 1.1rem;
  background: var(--warn-bg);
  border: 1px solid var(--warn-border);
  border-radius: var(--radius-sm);
}
.clarification .question {
  margin: 0 0 0.6rem;
}
.clarification form {
  display: flex;
  gap: 0.5rem;
}
.clarification input {
  flex: 1;
}
.clarified {
  color: var(--text-muted);
}
.cost {
  display: inline-block;
  margin: 0 0 1rem;
  padding: 0.4rem 0.7rem;
  color: var(--text-muted);
  font-size: 0.85rem;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
.cost strong {
  color: var(--text);
}
.loading {
  color: var(--text-muted);
}
</style>
