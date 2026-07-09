<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type SearchJobDetail } from "@/api";
import ResultList from "@/components/ResultList.vue";
import { useAuthStore } from "@/stores/auth";

const props = defineProps<{ id: string }>();
const auth = useAuthStore();
const router = useRouter();

const job = ref<SearchJobDetail | null>(null);
let timer: ReturnType<typeof setInterval> | undefined;

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
  if (job.value.status === "completed" || job.value.status === "failed") {
    clearInterval(timer);
  }
}

onMounted(() => {
  refresh();
  timer = setInterval(refresh, 2500);
});
onBeforeUnmount(() => clearInterval(timer));
</script>

<template>
  <section v-if="job">
    <h2>“{{ job.keyword }}”</h2>
    <p>
      Status: <strong>{{ job.status }}</strong>
      <span v-if="job.status === 'pending' || job.status === 'running'"> — refreshing…</span>
    </p>
    <p v-if="job.error" class="error">{{ job.error }}</p>
    <ResultList v-if="job.status === 'completed'" :results="job.results" />
  </section>
  <p v-else>Loading…</p>
</template>
