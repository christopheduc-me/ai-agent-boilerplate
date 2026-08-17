<script setup lang="ts">
// Render-error boundary (ADR-073). API failures are already handled where they
// happen — every view has its own error state, and `withAuth` retries after a
// refresh (ADR-008). This catches the other kind: an exception thrown *during
// render or a lifecycle hook*, which Vue answers by unmounting the whole tree.
// Without a boundary the user gets a blank page with nothing to act on.
//
// Scoped to the routed view on purpose: the topbar stays usable, so the user
// can still log out or navigate away instead of reloading blindly.
import { onErrorCaptured, ref } from "vue";
import { useRouter } from "vue-router";

const failed = ref(false);
const router = useRouter();

onErrorCaptured((error, _instance, info) => {
  failed.value = true;
  // `info` is Vue's own hook name ("render", "setup", ...) — the single most
  // useful field when reading this back from a user's console.
  console.error(`[ErrorBoundary] ${info}`, error);
  return false; // stop here: the boundary is now showing the fallback
});

// A route change replaces the subtree that failed, so recovery is just
// re-rendering it — no reload, and the session survives.
function retry(): void {
  failed.value = false;
}

function goHome(): void {
  failed.value = false;
  router.push({ name: "home" });
}
</script>

<template>
  <div v-if="failed" class="boundary" role="alert" data-testid="error-boundary">
    <h2>Something went wrong on this page</h2>
    <p>
      The rest of the app still works — your session is intact. Try this view
      again, or go back to your searches.
    </p>
    <div class="boundary-actions">
      <button type="button" data-testid="error-boundary-retry" @click="retry">Try again</button>
      <button type="button" class="btn-ghost" data-testid="error-boundary-home" @click="goHome">
        Back to searches
      </button>
    </div>
  </div>
  <slot v-else />
</template>

<style scoped>
.boundary {
  max-width: 34rem;
  margin: 2rem auto;
  padding: 1.25rem 1.5rem;
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  box-shadow: var(--shadow-sm);
}
.boundary h2 {
  margin-top: 0;
  font-size: 1.1rem;
}
.boundary p {
  color: var(--text-muted);
}
.boundary-actions {
  display: flex;
  gap: 0.5rem;
  margin-top: 1rem;
}
</style>
