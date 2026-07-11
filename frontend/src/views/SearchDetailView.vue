<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type SearchJobDetail } from "@/api";
import ResultTimeline from "@/components/ResultTimeline.vue";
import { useAuthStore } from "@/stores/auth";

const props = defineProps<{ id: string }>();
const auth = useAuthStore();
const router = useRouter();

const job = ref<SearchJobDetail | null>(null);
let timer: ReturnType<typeof setInterval> | undefined;
const abort = new AbortController();

function isTerminal(): boolean {
  return job.value?.status === "completed" || job.value?.status === "failed";
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
    <ResultTimeline v-if="job.status === 'completed'" :results="job.results" />
  </section>
  <p v-else>Loading…</p>
</template>
