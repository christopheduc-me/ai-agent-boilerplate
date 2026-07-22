<script setup lang="ts">
// Single-page workbench (ADR-039): launch either mode, follow the run, browse
// past runs and manage recurring searches — all in place, no navigation.
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type JobMode, type RecurringSearch, type SearchJob } from "@/api";
import RunPanel from "@/components/RunPanel.vue";
import StatusPill from "@/components/StatusPill.vue";
import { useAuthStore } from "@/stores/auth";
import { timeAgo } from "@/time";

const auth = useAuthStore();
const router = useRouter();

const workflowKeyword = ref("");
const agentKeyword = ref("");
const jobs = ref<SearchJob[]>([]);
const error = ref<string | null>(null);
const busy = ref(false);
// The run displayed in the inline panel (ADR-039).
const activeRunId = ref<string | null>(null);

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

async function launch(keyword: string, mode: JobMode): Promise<void> {
  error.value = null;
  busy.value = true;
  try {
    const { job_id } = await auth.withAuth((token) => api.launchSearch(keyword, token, mode));
    activeRunId.value = job_id; // stays on this page: the panel follows it live
    await refresh();
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

// Spend tracking (ADR-038): the sum of every listed run.
const totalCost = computed(() => jobs.value.reduce((sum, job) => sum + job.usage.cost_usd, 0));

// Ops consoles (ADR-040): dev-stack UIs published on fixed host ports by the
// compose observability profile. Same host as the app so remote dev works.
const opsConsoles = [
  { name: "Flower", role: "Celery workers & tasks", port: 5555 },
  { name: "Jaeger", role: "distributed traces", port: 16686 },
].map((c) => ({ ...c, url: `http://${window.location.hostname}:${c.port}` }));

onMounted(refresh);
</script>

<template>
  <div class="workbench">
    <aside class="controls scroll-pane">
      <!-- Two demos, one plumbing (ADR-030): the fixed pipeline vs the agentic
           loop where the LLM decides the queries and when to stop. -->
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
        <button type="submit" :disabled="busy">{{ busy ? "Launching…" : "Run the workflow" }}</button>
      </form>

      <form class="demo" data-testid="agent-demo" @submit.prevent="launch(agentKeyword, 'agent')">
        <h2>Agent demo</h2>
        <p class="pitch">
          An agentic loop: the LLM picks its own queries, judges coverage, refines and decides when
          to stop — watch its decision journal live.
        </p>
        <input v-model="agentKeyword" placeholder="Goal, e.g. rust hexagonal architecture" required />
        <button type="submit" :disabled="busy">{{ busy ? "Launching…" : "Run the agent" }}</button>
      </form>
      <p v-if="error" class="error">{{ error }}</p>

      <!-- Recurring searches with memory (ADR-033). -->
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
            <span v-if="search.last_run_at" class="muted">
              (last run {{ timeAgo(search.last_run_at) }})</span
            >
            <span v-else class="muted"> (first run pending)</span>
            <span v-if="search.webhook_url" class="muted" :title="search.webhook_url">
              📣 digest webhook</span
            >
            <button type="button" class="delete" @click="removeRecurring(search.id)">Delete</button>
          </li>
        </ul>
        <p v-if="recurring.length === 0" class="pitch">Nothing watched yet.</p>
      </section>

      <!-- History: pick a run, it loads in the panel — no navigation (ADR-039). -->
      <section class="history">
        <h2>Previous searches</h2>
        <p v-if="jobs.length > 0" class="muted" data-testid="total-cost">
          Total API spend: ${{ totalCost.toFixed(4) }}
        </p>
        <ul>
          <li
            v-for="job in jobs"
            :key="job.id"
            :class="{
              selected: job.id === activeRunId,
              attention: job.status === 'awaiting_input',
            }"
          >
            <button type="button" class="job-link" :title="job.keyword" @click="activeRunId = job.id">
              {{ job.keyword }}
            </button>
            <span class="mode" :data-mode="job.mode">{{ job.mode }}</span>
            <StatusPill :status="job.status" />
            <span class="muted">{{ timeAgo(job.created_at) }}</span>
            <span v-if="job.usage.cost_usd > 0" class="muted"
              >${{ job.usage.cost_usd.toFixed(4) }}</span
            >
          </li>
        </ul>
        <p v-if="jobs.length === 0" class="muted">No search yet.</p>
      </section>

      <!-- Ops consoles (ADR-040): the dev stack's monitoring UIs. -->
      <section class="ops" data-testid="ops-consoles">
        <h2>Ops consoles</h2>
        <p class="pitch">
          Dev-stack monitoring UIs — start them with
          <code>docker compose --profile observability up -d</code>.
        </p>
        <ul>
          <li v-for="console in opsConsoles" :key="console.name">
            <a :href="console.url" target="_blank" rel="noopener">{{ console.name }}</a>
            <span class="muted"> — {{ console.role }}</span>
          </li>
        </ul>
      </section>
    </aside>

    <main class="stage scroll-pane">
      <RunPanel v-if="activeRunId" :id="activeRunId" @finished="refresh" />
      <div v-else class="empty-stage" data-testid="empty-stage">
        <p>Launch a workflow or agent run — it will play out right here, live.</p>
        <p class="muted">Or pick a previous search from the history.</p>
      </div>
    </main>
  </div>
</template>

<style scoped>
.workbench {
  display: grid;
  grid-template-columns: minmax(360px, 430px) 1fr;
  gap: 1.5rem;
  align-items: start;
}
/* Each column scrolls on its own, bounded to the viewport, so the run panel
   (with its long timeline) never pushes the launchers off-screen. */
.controls,
.stage {
  max-height: calc(100dvh - 5.5rem);
  overflow-y: auto;
}
.controls {
  display: flex;
  flex-direction: column;
  gap: 1rem;
  padding-right: 0.4rem;
}
.stage {
  min-height: 420px;
  padding: 1.75rem 2rem;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  box-shadow: var(--shadow-sm);
}
@media (max-width: 860px) {
  .workbench {
    grid-template-columns: 1fr;
  }
  .controls,
  .stage {
    max-height: none;
    overflow: visible;
  }
  .controls {
    padding-right: 0;
  }
}
.empty-stage {
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: center;
  min-height: 280px;
  color: var(--text-muted);
  text-align: center;
  gap: 0.25rem;
}
.empty-stage p:first-child {
  font-size: 1.05rem;
  color: var(--text);
  font-weight: 500;
}
.demo,
.recurring,
.history,
.ops {
  padding: 1.35rem 1.5rem;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  box-shadow: var(--shadow-sm);
}
.demo {
  display: flex;
  flex-direction: column;
  gap: 0.7rem;
}
.demo h2,
.recurring h2,
.history h2,
.ops h2 {
  margin: 0;
  font-size: 1.05rem;
  font-weight: 700;
  letter-spacing: -0.01em;
}
.demo[data-testid="agent-demo"] {
  border-color: color-mix(in srgb, var(--accent) 30%, var(--border));
  background: linear-gradient(180deg, var(--accent-soft) 0%, var(--surface) 55%);
}
.pitch {
  margin: 0;
  color: var(--text-muted);
  font-size: 0.88rem;
}
.mode {
  display: inline-block;
  font-size: 0.66rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  padding: 0.1rem 0.45rem;
  margin-left: 0.4rem;
  border-radius: 999px;
  background: var(--surface-2);
  border: 1px solid var(--border-strong);
  color: var(--text-muted);
}
.mode[data-mode="agent"] {
  background: var(--accent-soft);
  border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  color: var(--accent);
}
.recurring form {
  display: flex;
  gap: 0.5rem;
  align-items: center;
  flex-wrap: wrap;
  margin-top: 0.75rem;
}
.recurring form input:first-child {
  flex: 1;
  min-width: 140px;
}
.recurring select,
.interval input {
  width: auto;
}
.interval {
  display: flex;
  gap: 0.3rem;
  align-items: center;
  font-size: 0.85rem;
  color: var(--text-muted);
}
.interval input {
  width: 4.5rem;
}
.recurring ul,
.history ul,
.ops ul {
  list-style: none;
  margin: 0.75rem 0 0;
  padding: 0;
}
.ops li {
  padding: 0.25rem 0;
  font-size: 0.92rem;
}
.ops a {
  font-weight: 600;
}
.ops code {
  font-size: 0.8rem;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0.05rem 0.3rem;
}
.recurring li,
.history li {
  padding: 0.35rem 0.4rem;
  border-radius: var(--radius-sm);
  font-size: 0.92rem;
}
.history li {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  flex-wrap: wrap;
}
.history li + li,
.recurring li + li {
  border-top: 1px solid var(--border);
}
.history li.selected {
  background: var(--accent-soft);
  border-left: 3px solid var(--accent);
  padding-left: 0.5rem;
}
/* The agent is waiting on the user (ADR-032) — must not drown in the list. */
.history li.attention {
  background: var(--warn-bg);
  border-left: 3px solid var(--warn-border);
  padding-left: 0.5rem;
}
.job-link {
  background: none;
  border: none;
  padding: 0;
  color: var(--accent);
  cursor: pointer;
  font: inherit;
  font-weight: 600;
  /* Long keywords must not wrap the whole row — truncate with a tooltip. */
  flex: 1 1 auto;
  min-width: 0;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  text-align: left;
}
.job-link:hover {
  background: none;
  text-decoration: underline;
}
.muted {
  color: var(--text-muted);
  font-size: 0.85rem;
}
.delete {
  margin-left: 0.5rem;
  padding: 0.15rem 0.5rem;
  font-size: 0.78rem;
  font-weight: 600;
  color: var(--danger);
  background: transparent;
  border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent);
}
.delete:hover {
  background: color-mix(in srgb, var(--danger) 12%, transparent);
}
.webhook {
  flex: 1 1 100%;
}
</style>
