<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type JobMode, type SearchJob } from "@/api";
import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

const workflowKeyword = ref("");
const agentKeyword = ref("");
const jobs = ref<SearchJob[]>([]);
const error = ref<string | null>(null);
const busy = ref(false);

async function refresh(): Promise<void> {
  try {
    jobs.value = await auth.withAuth((token) => api.listSearches(token));
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) router.push({ name: "login" });
  }
}

async function launch(keyword: string, mode: JobMode): Promise<void> {
  error.value = null;
  busy.value = true;
  try {
    const { job_id } = await auth.withAuth((token) => api.launchSearch(keyword, token, mode));
    router.push({ name: "search-detail", params: { id: job_id } });
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      router.push({ name: "login" });
      return;
    }
    error.value = e instanceof ApiError ? e.message : "unexpected error";
  } finally {
    busy.value = false;
  }
}

onMounted(refresh);
</script>

<template>
  <section>
    <!-- Two demos, one plumbing (ADR-030): the fixed pipeline vs the agentic
         loop where the LLM decides the queries and when to stop. -->
    <div class="demos">
      <form
        class="demo"
        data-testid="workflow-demo"
        @submit.prevent="launch(workflowKeyword, 'workflow')"
      >
        <h2>Workflow demo</h2>
        <p class="pitch">
          A fixed pipeline: one search with your keyword, every result enriched and rendered as a
          timeline. Deterministic and cheap.
        </p>
        <input v-model="workflowKeyword" placeholder="Keyword, e.g. rust hexagonal architecture" required />
        <button type="submit" :disabled="busy">Run the workflow</button>
      </form>

      <form class="demo" data-testid="agent-demo" @submit.prevent="launch(agentKeyword, 'agent')">
        <h2>Agent demo</h2>
        <p class="pitch">
          An agentic loop: the LLM picks its own queries, judges coverage, refines and decides when
          to stop — watch its decision journal live.
        </p>
        <input v-model="agentKeyword" placeholder="Goal, e.g. rust hexagonal architecture" required />
        <button type="submit" :disabled="busy">Run the agent</button>
      </form>
    </div>
    <p v-if="error" class="error">{{ error }}</p>

    <h2>Previous searches</h2>
    <ul>
      <li v-for="job in jobs" :key="job.id">
        <RouterLink :to="{ name: 'search-detail', params: { id: job.id } }">
          {{ job.keyword }}
        </RouterLink>
        <span class="mode" :data-mode="job.mode">{{ job.mode }}</span>
        — {{ job.status }}
      </li>
    </ul>
    <p v-if="jobs.length === 0">No search yet.</p>
  </section>
</template>

<style scoped>
.demos {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 1rem;
  margin-bottom: 1rem;
}
.demo {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 1rem;
  border: 1px solid #dde5ee;
  border-radius: 6px;
  background: #f7f9fb;
}
.demo h2 {
  margin: 0;
}
.pitch {
  margin: 0;
  color: #555;
  font-size: 0.9rem;
  flex: 1;
}
.mode {
  font-size: 0.7rem;
  text-transform: uppercase;
  padding: 0.05rem 0.4rem;
  margin-left: 0.4rem;
  border-radius: 3px;
  background: #eef2f7;
  border: 1px solid #c7d4e2;
}
.mode[data-mode="agent"] {
  background: #eaf6ee;
  border-color: #b7dcc2;
}
</style>
