<script setup lang="ts">
// Profile (ADR-061): the account e-mail and the notification channels where the
// user receives digests (Slack / Telegram), chosen once and reused by every
// recurring search.
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import {
  api,
  ApiError,
  type Channel,
  type ChannelKind,
  type KnowledgeDocument,
  type Profile,
} from "@/api";
import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

const profile = ref<Profile | null>(null);
const error = ref<string | null>(null);
const busy = ref(false);

// Knowledge base (ADR-063): upload local text/markdown files (or a folder).
const documents = ref<KnowledgeDocument[]>([]);
const docError = ref<string | null>(null);
const docNote = ref<string | null>(null);
const docBusy = ref(false);

// V1 is text/markdown: read files client-side and send their text. Binary files
// (PDF…) are skipped — parsing them is a documented extension.
const ACCEPT = ".txt,.md,.markdown,.text,.csv,.json,.log,.rst,.mdx";
const TEXT_EXT = /\.(txt|md|markdown|text|csv|json|log|rst|mdx)$/i;
const MAX_BYTES = 200_000; // matches the backend's content cap

// FileReader (not File.text(), which jsdom lacks) — works in browsers and tests.
function readText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(reader.error ?? new Error("read failed"));
    reader.readAsText(file);
  });
}

const kind = ref<ChannelKind>("slack");
const target = ref("");
const secret = ref("");

// Slack: webhook URL; Telegram: chat id + bot token; Email: an address.
const targetLabel = computed(() => {
  if (kind.value === "slack") return "Slack incoming-webhook URL";
  if (kind.value === "telegram") return "Telegram chat id";
  return "Email address";
});
const needsSecret = computed(() => kind.value === "telegram");
const emailEnabled = computed(() => profile.value?.email_enabled ?? false);

async function load(): Promise<void> {
  try {
    profile.value = await auth.withAuth((token) => api.getProfile(token));
    documents.value = await auth.withAuth((token) => api.listDocuments(token));
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) router.push({ name: "login" });
  }
}

async function onFiles(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const files = Array.from(input.files ?? []);
  input.value = ""; // allow re-selecting the same file/folder later
  if (files.length === 0) return;

  docError.value = null;
  docNote.value = null;
  docBusy.value = true;
  let uploaded = 0;
  let skipped = 0;
  try {
    for (const file of files) {
      // Keep the folder path as the name so subfolders stay distinguishable.
      const name = file.webkitRelativePath || file.name;
      if (!TEXT_EXT.test(file.name) || file.size > MAX_BYTES) {
        skipped++;
        continue;
      }
      const content = (await readText(file)).trim();
      if (!content) {
        skipped++;
        continue;
      }
      await auth.withAuth((token) => api.uploadDocument(name, content, token));
      uploaded++;
    }
    await load();
    docNote.value =
      `Uploaded ${uploaded} document(s)` +
      (skipped ? `, skipped ${skipped} (non-text or too large)` : "");
  } catch (e) {
    docError.value = e instanceof ApiError ? e.message : "unexpected error";
  } finally {
    docBusy.value = false;
  }
}

async function removeDocument(doc: KnowledgeDocument): Promise<void> {
  try {
    await auth.withAuth((token) => api.deleteDocument(doc.id, token));
    await load();
  } catch (e) {
    docError.value = e instanceof ApiError ? e.message : "unexpected error";
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
          <option v-if="emailEnabled" value="email">Email</option>
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

    <h3>Knowledge base</h3>
    <p class="muted">
      Upload text or markdown files from your computer to ground the agent's reasoning on your own
      material (ADR-063). Each is embedded in the background — usable once <em>ready</em>.
      Non-text files (PDF…) are skipped.
    </p>

    <ul v-if="documents.length > 0" class="documents" data-testid="document-list">
      <li v-for="doc in documents" :key="doc.id" :data-testid="`document-${doc.id}`">
        <span class="badge" :class="`status-${doc.status}`">{{ doc.status }}</span>
        <span class="doc-name" :title="doc.error ?? doc.name">{{ doc.name }}</span>
        <button type="button" class="delete" @click="removeDocument(doc)">Delete</button>
      </li>
    </ul>
    <p v-else class="muted" data-testid="no-documents">No documents yet.</p>

    <div class="add-document" data-testid="add-document">
      <label class="filebtn">
        Choose files…
        <input type="file" multiple :accept="ACCEPT" data-testid="doc-files" @change="onFiles" />
      </label>
      <label class="filebtn">
        Choose folder…
        <input
          type="file"
          webkitdirectory
          :accept="ACCEPT"
          data-testid="doc-folder"
          @change="onFiles"
        />
      </label>
      <span v-if="docBusy" class="muted" data-testid="document-uploading">Uploading…</span>
    </div>
    <p v-if="docNote" class="muted" data-testid="document-note">{{ docNote }}</p>
    <p v-if="docError" class="error" data-testid="document-error">{{ docError }}</p>
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
.add-document {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  margin-top: 1rem;
}
/* A styled label acting as a button, with the real file input hidden inside. */
.filebtn {
  display: inline-flex;
  align-items: center;
  padding: 0.5rem 0.9rem;
  font-weight: 600;
  cursor: pointer;
  color: var(--accent-contrast);
  background: var(--accent);
  border-radius: var(--radius-sm);
}
.filebtn:hover {
  background: var(--accent-hover);
}
.filebtn input[type="file"] {
  display: none;
}
.documents {
  list-style: none;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}
.documents li {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.5rem 0.65rem;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
.doc-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.badge.status-ready {
  background: var(--success-bg);
  color: var(--success);
}
.badge.status-failed {
  background: color-mix(in srgb, var(--danger) 14%, transparent);
  color: var(--danger);
}
.error {
  color: var(--danger);
}
</style>
