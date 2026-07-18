<script setup lang="ts">
// Timeline rendering of research results (ADR-027). Results arrive sorted
// newest first (ADR-011); this component only groups and presents them:
// - dated results grouped by month, solid marker for provider-confirmed dates,
//   hollow marker + "(estimated)" for LLM-extracted ones (date_confidence);
// - the event_type as a badge, the LLM summary when present;
// - undated results in a separate section, never mixed in.
import { computed } from "vue";

import type { SearchResult } from "@/api";

// `highlightNew` is set on recurring-search runs (ADR-033): results already
// seen by previous runs are dimmed, new ones get a chip.
const props = defineProps<{ results: SearchResult[]; highlightNew?: boolean }>();

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
          :class="[`confidence-${result.date_confidence}`, { seen: highlightNew && !result.is_new }]"
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
              <span v-if="highlightNew && result.is_new" class="new-chip">new</span>
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
        <li
          v-for="result in undated"
          :key="result.url"
          class="entry confidence-unknown"
          :class="{ seen: highlightNew && !result.is_new }"
        >
          <span class="marker" aria-hidden="true" />
          <div class="content">
            <p class="meta">
              <span class="badge" :data-event="result.event_type">{{ result.event_type }}</span>
              <span v-if="highlightNew && result.is_new" class="new-chip">new</span>
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
  border-left: 2px solid var(--border-strong);
}
.month {
  margin: 1.5rem 0 0.6rem;
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
}
.entry {
  position: relative;
  padding: 0 0 1.15rem 1.35rem;
}
.marker {
  position: absolute;
  left: -7px;
  top: 0.3rem;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  border: 2px solid var(--accent);
  background: var(--accent);
  box-shadow: 0 0 0 3px var(--surface);
}
.confidence-medium .marker {
  background: var(--surface); /* hollow = LLM-estimated date */
}
.confidence-unknown .marker {
  background: var(--surface);
  border-color: var(--text-muted);
  border-style: dashed;
}
.meta {
  margin: 0 0 0.15rem;
  font-size: 0.8rem;
  color: var(--text-muted);
  display: flex;
  gap: 0.5rem;
  align-items: center;
  flex-wrap: wrap;
}
.estimated {
  font-style: italic;
}
.content a {
  font-weight: 600;
}
.badge {
  font-size: 0.66rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.02em;
  padding: 0.08rem 0.45rem;
  border-radius: 999px;
  background: var(--surface-2);
  border: 1px solid var(--border-strong);
  color: var(--text-muted);
}
.summary {
  margin: 0.15rem 0 0;
  color: var(--text);
  font-size: 0.92rem;
}
.new-chip {
  font-size: 0.66rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.02em;
  padding: 0.08rem 0.45rem;
  border-radius: 999px;
  background: var(--success-bg);
  border: 1px solid var(--success-border);
  color: var(--success);
}
.entry.seen .content {
  opacity: 0.5; /* already delivered by a previous run (ADR-033) */
}
</style>
