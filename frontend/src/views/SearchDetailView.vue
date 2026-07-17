<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type SearchJobDetail } from "@/api";
import AgentJournal from "@/components/AgentJournal.vue";
import ResultTimeline from "@/components/ResultTimeline.vue";
import { useAuthStore } from "@/stores/auth";

const props = defineProps<{ id: string }>();
const auth = useAuthStore();
const router = useRouter();

const job = ref<SearchJobDetail | null>(null);
const answer = ref("");
const answerError = ref<string | null>(null);
const answering = ref(false);
let timer: ReturnType<typeof setInterval> | undefined;
const abort = new AbortController();

function isTerminal(): boolean {
  return job.value?.status === "completed" || job.value?.status === "failed";
}

/** Sends the clarification answer (ADR-032); the job resumes and the live
 *  updates (SSE or polling) carry it through to completion. */
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

async function refresh(): Promise<void> {
  try {
    job.value = await auth.withAuth((token) => api.getSearch(props.id, token));
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      clearInterval(timer);
      router.push({ name: "login" });
    }
    return;
  }
  // Poll while the agent works (ADR-003); stop on a terminal status.
  if (isTerminal()) clearInterval(timer);
}

function startPolling(): void {
  refresh();
  timer = setInterval(refresh, 2500);
}

onMounted(async () => {
  // Live updates over SSE (ADR-026); the server closes the stream after the
  // terminal status. Any failure falls back to plain polling.
  try {
    await auth.withAuth((token) =>
      api.streamSearch(props.id, token, (update) => (job.value = update), abort.signal),
    );
    if (!isTerminal()) startPolling(); // stream ended early: keep following
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      router.push({ name: "login" });
      return;
    }
    if (!abort.signal.aborted) startPolling();
  }
});
onBeforeUnmount(() => {
  abort.abort();
  clearInterval(timer);
});
</script>

<template>
  <section v-if="job">
    <h2>“{{ job.keyword }}”</h2>
    <p>
      Status: <strong>{{ job.status }}</strong>
      <span v-if="job.status === 'pending' || job.status === 'running'"> — live…</span>
    </p>
    <p v-if="job.error" class="error">{{ job.error }}</p>
    <!-- API spend of this run (ADR-038); $0 with the fake providers. -->
    <p
      v-if="job.usage.llm_calls > 0 || job.usage.search_calls > 0"
      class="cost"
      data-testid="job-cost"
    >
      💸 ${{ job.usage.cost_usd.toFixed(4) }} — {{ job.usage.llm_calls }} LLM call{{
        job.usage.llm_calls === 1 ? "" : "s"
      }}
      ({{ job.usage.llm_input_tokens }} in / {{ job.usage.llm_output_tokens }} out tokens),
      {{ job.usage.search_calls }} search{{ job.usage.search_calls === 1 ? "" : "es" }}
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
  <p v-else>Loading…</p>
</template>

<style scoped>
.clarification {
  margin: 1rem 0;
  padding: 0.75rem 1rem;
  background: #fff8e6;
  border: 1px solid #e8d8a0;
  border-radius: 6px;
}
.clarification .question {
  margin: 0 0 0.5rem;
}
.clarification form {
  display: flex;
  gap: 0.5rem;
}
.clarification input {
  flex: 1;
}
.clarified {
  color: #666;
}
.cost {
  color: #666;
  font-size: 0.85rem;
}
</style>
