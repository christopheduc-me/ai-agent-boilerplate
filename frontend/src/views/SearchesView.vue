<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type SearchJob } from "@/api";
import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

const keyword = ref("");
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

async function launch(): Promise<void> {
  error.value = null;
  busy.value = true;
  try {
    const { job_id } = await auth.withAuth((token) => api.launchSearch(keyword.value, token));
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
    <h2>Launch a research</h2>
    <form @submit.prevent="launch">
      <input v-model="keyword" placeholder="Keyword, e.g. rust hexagonal architecture" required />
      <button type="submit" :disabled="busy">Search</button>
    </form>
    <p v-if="error" class="error">{{ error }}</p>

    <h2>Previous searches</h2>
    <ul>
      <li v-for="job in jobs" :key="job.id">
        <RouterLink :to="{ name: 'search-detail', params: { id: job.id } }">
          {{ job.keyword }}
        </RouterLink>
        — {{ job.status }}
      </li>
    </ul>
    <p v-if="jobs.length === 0">No search yet.</p>
  </section>
</template>

<style scoped>
form {
  display: flex;
  gap: 0.5rem;
}
form input {
  flex: 1;
}
</style>
