<script setup lang="ts">
// Timeline rendering of research results (ADR-027). Results arrive sorted
// newest first (ADR-011); this component only groups and presents them:
// - dated results grouped by month, solid marker for provider-confirmed dates,
//   hollow marker + "(estimated)" for LLM-extracted ones (date_confidence);
// - the event_type as a badge, the LLM summary when present;
// - undated results in a separate section, never mixed in.
import { computed } from "vue";

import type { SearchResult } from "@/api";

const props = defineProps<{ results: SearchResult[] }>();

interface MonthGroup {
  label: string;
  items: SearchResult[];
}

const monthLabel = (iso: string): string =>
  new Date(iso).toLocaleDateString("en-US", { month: "long", year: "numeric", timeZone: "UTC" });

const dayLabel = (iso: string): string => new Date(iso).toISOString().slice(0, 10);

const dated = computed<MonthGroup[]>(() => {
  const groups: MonthGroup[] = [];
  for (const result of props.results) {
    if (result.published_at === null) continue;
    const label = monthLabel(result.published_at);
    const last = groups[groups.length - 1];
    if (last && last.label === label) last.items.push(result);
    else groups.push({ label, items: [result] });
  }
  return groups;
});

const undated = computed(() => props.results.filter((r) => r.published_at === null));
</script>

<template>
  <div class="timeline">
    <section v-for="group in dated" :key="group.label" :data-testid="`month-${group.label}`">
      <h3 class="month">{{ group.label }}</h3>
      <ol>
        <li
          v-for="result in group.items"
          :key="result.url"
          class="entry"
          :class="`confidence-${result.date_confidence}`"
        >
          <span class="marker" aria-hidden="true" />
          <div class="content">
            <p class="meta">
              <time :datetime="result.published_at ?? undefined">
                {{ dayLabel(result.published_at as string) }}
              </time>
              <span v-if="result.date_confidence === 'medium'" class="estimated">
                (estimated)
              </span>
              <span class="badge" :data-event="result.event_type">{{ result.event_type }}</span>
            </p>
            <a :href="result.url" target="_blank" rel="noopener">{{ result.title }}</a>
            <p class="summary">{{ result.summary ?? result.snippet }}</p>
          </div>
        </li>
      </ol>
    </section>

    <section v-if="undated.length > 0" data-testid="unknown-date-section">
      <h3 class="month">Unknown date</h3>
      <ul>
        <li v-for="result in undated" :key="result.url" class="entry confidence-unknown">
          <span class="marker" aria-hidden="true" />
          <div class="content">
            <p class="meta">
              <span class="badge" :data-event="result.event_type">{{ result.event_type }}</span>
            </p>
            <a :href="result.url" target="_blank" rel="noopener">{{ result.title }}</a>
            <p class="summary">{{ result.summary ?? result.snippet }}</p>
          </div>
        </li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.timeline ol,
.timeline ul {
  list-style: none;
  margin: 0;
  padding: 0;
  border-left: 2px solid #d0d0d0;
}
.month {
  margin: 1.25rem 0 0.5rem;
  font-size: 0.9rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: #666;
}
.entry {
  position: relative;
  padding: 0 0 1rem 1.25rem;
}
.marker {
  position: absolute;
  left: -7px;
  top: 0.3rem;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  border: 2px solid #3a6ea5;
  background: #3a6ea5;
}
.confidence-medium .marker {
  background: transparent; /* hollow = LLM-estimated date */
}
.confidence-unknown .marker {
  background: transparent;
  border-color: #aaa;
  border-style: dashed;
}
.meta {
  margin: 0;
  font-size: 0.85rem;
  color: #666;
  display: flex;
  gap: 0.5rem;
  align-items: baseline;
}
.estimated {
  font-style: italic;
}
.badge {
  font-size: 0.7rem;
  text-transform: uppercase;
  padding: 0.05rem 0.4rem;
  border-radius: 3px;
  background: #eef2f7;
  border: 1px solid #c7d4e2;
}
.summary {
  margin: 0.15rem 0 0;
  color: #444;
}
</style>
