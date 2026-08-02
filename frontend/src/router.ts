import { createRouter, createWebHistory } from "vue-router";

import { useAuthStore } from "@/stores/auth";
import LoginView from "@/views/LoginView.vue";
import ProfileView from "@/views/ProfileView.vue";
import SearchesView from "@/views/SearchesView.vue";

// Single-page workbench (ADR-039): runs play out inline on the searches page,
// so there is no per-search route anymore. The profile (ADR-061) is the one
// extra page — notification channel management.
export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/login", name: "login", component: LoginView },
    { path: "/", name: "searches", component: SearchesView },
    { path: "/profile", name: "profile", component: ProfileView },
  ],
});

router.beforeEach(async (to) => {
  const auth = useAuthStore();
  if (to.name !== "login" && !auth.isAuthenticated) {
    // Silent restore: the access token is memory-only, but the refresh cookie
    // survives page reloads (ADR-008).
    if (await auth.tryRefresh()) return;
    return { name: "login" };
  }
});
