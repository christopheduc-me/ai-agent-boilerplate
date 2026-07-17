<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type JobMode, type RecurringSearch, type SearchJob } from "@/api";
import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

const workflowKeyword = ref("");
const agentKeyword = ref("");
const jobs = ref<SearchJob[]>([]);
const error = ref<string | null>(null);
const busy = ref(false);

// Recurring searches (ADR-033).
const recurring = ref<RecurringSearch[]>([]);
const recurringKeyword = ref("");
const recurringMode = ref<JobMode>("agent");
const recurringInterval = ref(60);
const recurringWebhook = ref("");
const recurringError = ref<string | null>(null);

async function refresh(): Promise<void> {
  try {
    jobs.value = await auth.withAuth((token) => api.listSearches(token));
    recurring.value = await auth.withAuth((token) => api.listRecurring(token));
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) router.push({ name: "login" });
  }
}

async function createRecurring(): Promise<void> {
  recurringError.value = null;
  try {
    await auth.withAuth((token) =>
      api.createRecurring(
        recurringKeyword.value,
        recurringMode.value,
        recurringInterval.value,
        token,
        recurringWebhook.value,
      ),
    );
    recurringKeyword.value = "";
    recurringWebhook.value = "";
    await refresh();
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      router.push({ name: "login" });
      return;
    }
    recurringError.value = e instanceof ApiError ? e.message : "unexpected error";
  }
}

async function removeRecurring(id: string): Promise<void> {
  try {
    await auth.withAuth((token) => api.deleteRecurring(id, token));
    await refresh();
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

// Spend tracking (ADR-038): the sum of every listed run.
const totalCost = computed(() => jobs.value.reduce((sum, job) => sum + job.usage.cost_usd, 0));

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

    <!-- Recurring searches with memory (ADR-033): the backend scheduler
         re-runs them; results are flagged new/seen against previous runs. -->
    <section class="recurring" data-testid="recurring-section">
      <h2>Recurring searches</h2>
      <p class="pitch">
        Saved searches re-run automatically. Each run remembers the previous ones: results are
        flagged <strong>new</strong> or already seen, and the agent reports the delta.
      </p>
      <form data-testid="recurring-form" @submit.prevent="createRecurring">
        <input v-model="recurringKeyword" placeholder="Keyword to watch" required />
        <select v-model="recurringMode" aria-label="Mode">
          <option value="agent">agent</option>
          <option value="workflow">workflow</option>
        </select>
        <label class="interval">
          every
          <input v-model.number="recurringInterval" type="number" min="1" max="10080" required />
          min
        </label>
        <input
          v-model="recurringWebhook"
          class="webhook"
          type="url"
          placeholder="Webhook URL for digests (optional)"
        />
        <button type="submit">Watch</button>
      </form>
      <p v-if="recurringError" class="error">{{ recurringError }}</p>
      <ul>
        <li v-for="search in recurring" :key="search.id" :data-testid="`recurring-${search.id}`">
          <strong>{{ search.keyword }}</strong>
          <span class="mode" :data-mode="search.mode">{{ search.mode }}</span>
          — every {{ search.interval_minutes }} min
          <span v-if="search.last_run_at" class="last-run">
            (last run {{ new Date(search.last_run_at).toLocaleString() }})</span
          >
          <span v-else class="last-run"> (first run pending)</span>
          <span v-if="search.webhook_url" class="last-run" :title="search.webhook_url">
            📣 digest webhook</span
          >
          <button type="button" class="delete" @click="removeRecurring(search.id)">Delete</button>
        </li>
      </ul>
      <p v-if="recurring.length === 0" class="pitch">Nothing watched yet.</p>
    </section>

    <h2>Previous searches</h2>
    <p v-if="jobs.length > 0" class="total-cost" data-testid="total-cost">
      Total API spend: ${{ totalCost.toFixed(4) }}
    </p>
    <ul>
      <li v-for="job in jobs" :key="job.id">
        <RouterLink :to="{ name: 'search-detail', params: { id: job.id } }">
          {{ job.keyword }}
        </RouterLink>
        <span class="mode" :data-mode="job.mode">{{ job.mode }}</span>
        — {{ job.status }}
        <span v-if="job.usage.cost_usd > 0" class="job-cost"
          >— ${{ job.usage.cost_usd.toFixed(4) }}</span
        >
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
.recurring {
  margin: 1rem 0;
  padding: 1rem;
  border: 1px solid #dde5ee;
  border-radius: 6px;
}
.recurring h2 {
  margin: 0 0 0.25rem;
}
.recurring form {
  display: flex;
  gap: 0.5rem;
  align-items: center;
  flex-wrap: wrap;
  margin-top: 0.5rem;
}
.recurring form input:first-child {
  flex: 1;
  min-width: 160px;
}
.interval {
  display: flex;
  gap: 0.3rem;
  align-items: center;
}
.interval input {
  width: 4.5rem;
}
.recurring ul {
  list-style: none;
  margin: 0.5rem 0 0;
  padding: 0;
}
.recurring li {
  padding: 0.25rem 0;
}
.last-run {
  color: #666;
  font-size: 0.85rem;
}
.delete {
  margin-left: 0.6rem;
  font-size: 0.8rem;
}
.total-cost,
.job-cost {
  color: #666;
  font-size: 0.85rem;
}
.total-cost {
  margin: 0 0 0.5rem;
}
</style>
