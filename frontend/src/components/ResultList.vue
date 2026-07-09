<script setup lang="ts">
import { computed } from "vue";

import type { SearchResult } from "@/api";

const props = defineProps<{ results: SearchResult[] }>();

// The backend sorts by publication date; here we only split the display:
// dated results first, then an "unknown date" section (ADR-011).
const dated = computed(() => props.results.filter((r) => r.published_at !== null));
const undated = computed(() => props.results.filter((r) => r.published_at === null));

function formatDate(iso: string): string {
  return new Date(iso).toISOString().slice(0, 10);
}
</script>

<template>
  <div>
    <ol class="results">
      <li v-for="result in dated" :key="result.url">
        <a :href="result.url" target="_blank" rel="noopener">{{ result.title }}</a>
        <span class="date">
          {{ formatDate(result.published_at!) }}
          <em v-if="result.date_confidence === 'medium'">(estimated)</em>
        </span>
        <p>{{ result.snippet }}</p>
      </li>
    </ol>

    <section v-if="undated.length > 0" data-testid="unknown-date-section">
      <h3>Unknown publication date</h3>
      <ul class="results">
        <li v-for="result in undated" :key="result.url">
          <a :href="result.url" target="_blank" rel="noopener">{{ result.title }}</a>
          <p>{{ result.snippet }}</p>
        </li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.results li {
  margin-bottom: 0.75rem;
}
.date {
  margin-left: 0.5rem;
  color: #555;
  font-size: 0.9rem;
}
</style>
