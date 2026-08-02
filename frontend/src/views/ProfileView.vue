<script setup lang="ts">
// Profile (ADR-061): the account e-mail and the notification channels where the
// user receives digests (Slack / Telegram), chosen once and reused by every
// recurring search.
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { api, ApiError, type Channel, type ChannelKind, type Profile } from "@/api";
import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

const profile = ref<Profile | null>(null);
const error = ref<string | null>(null);
const busy = ref(false);

const kind = ref<ChannelKind>("slack");
const target = ref("");
const secret = ref("");

// Slack needs a webhook URL; Telegram needs a chat id + a bot token.
const targetLabel = computed(() =>
  kind.value === "slack" ? "Slack incoming-webhook URL" : "Telegram chat id",
);
const needsSecret = computed(() => kind.value === "telegram");

async function load(): Promise<void> {
  try {
    profile.value = await auth.withAuth((token) => api.getProfile(token));
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) router.push({ name: "login" });
  }
}

async function addChannel(): Promise<void> {
  error.value = null;
  busy.value = true;
  try {
    await auth.withAuth((token) =>
      api.addChannel(kind.value, target.value, token, secret.value || undefined),
    );
    target.value = "";
    secret.value = "";
    await load();
  } catch (e) {
    error.value = e instanceof ApiError ? e.message : "unexpected error";
  } finally {
    busy.value = false;
  }
}

async function removeChannel(channel: Channel): Promise<void> {
  try {
    await auth.withAuth((token) => api.deleteChannel(channel.id, token));
    await load();
  } catch (e) {
    error.value = e instanceof ApiError ? e.message : "unexpected error";
  }
}

onMounted(load);
</script>

<template>
  <section class="profile" data-testid="profile-view">
    <header class="profile-head">
      <h2>Profile</h2>
      <RouterLink :to="{ name: 'searches' }" class="back">← Back to searches</RouterLink>
    </header>

    <p v-if="profile" class="email" data-testid="profile-email">{{ profile.email }}</p>

    <h3>Where you receive results</h3>
    <p class="muted">
      Digests from your recurring searches are delivered to every channel below (in addition to a
      search's own webhook, if set).
    </p>

    <ul v-if="profile && profile.channels.length > 0" class="channels" data-testid="channel-list">
      <li v-for="channel in profile.channels" :key="channel.id" :data-testid="`channel-${channel.id}`">
        <span class="badge">{{ channel.kind }}</span>
        <span class="target" :title="channel.target">{{ channel.target }}</span>
        <button type="button" class="delete" @click="removeChannel(channel)">Delete</button>
      </li>
    </ul>
    <p v-else class="muted" data-testid="no-channels">No channels yet.</p>

    <form class="add-channel" data-testid="add-channel-form" @submit.prevent="addChannel">
      <label>
        Channel
        <select v-model="kind" data-testid="channel-kind">
          <option value="slack">Slack</option>
          <option value="telegram">Telegram</option>
        </select>
      </label>
      <label>
        {{ targetLabel }}
        <input v-model="target" :placeholder="targetLabel" required />
      </label>
      <label v-if="needsSecret">
        Telegram bot token
        <input v-model="secret" type="password" placeholder="Bot token" />
      </label>
      <button type="submit" :disabled="busy">{{ busy ? "Adding…" : "Add channel" }}</button>
    </form>
    <p v-if="error" class="error" data-testid="channel-error">{{ error }}</p>
  </section>
</template>

<style scoped>
.profile {
  max-width: 640px;
  margin: 0 auto;
}
.profile-head {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
}
.email {
  font-weight: 600;
}
.muted {
  color: var(--text-muted);
}
.channels {
  list-style: none;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}
.channels li {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.5rem 0.65rem;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
.channels .target {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: ui-monospace, monospace;
  font-size: 0.85rem;
}
.badge {
  text-transform: uppercase;
  font-size: 0.7rem;
  font-weight: 700;
  padding: 0.15rem 0.45rem;
  border-radius: 999px;
  background: var(--accent-soft);
  color: var(--accent);
}
.delete {
  background: transparent;
  color: var(--danger);
  border: 1px solid var(--border-strong);
}
.add-channel {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  margin-top: 1rem;
  padding: 1rem;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
.error {
  color: var(--danger);
}
</style>
