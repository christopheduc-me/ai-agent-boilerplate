import { createPinia } from "pinia";
import { createApp } from "vue";

import App from "@/App.vue";
import { router } from "@/router";

const app = createApp(App);

// Last-resort handler (ADR-073). ErrorBoundary catches what happens inside the
// routed view and shows a fallback; this catches what it cannot — errors thrown
// in the topbar, in a store action, or in a watcher outside the boundary's
// subtree. It only logs: the app is the only tier of this project where an
// exception would otherwise vanish without a trace, while the backend records
// everything with a correlation id (ADR-018).
//
// There is no telemetry sink on purpose — picking one is a fork's decision.
// Ship errors somewhere by replacing this body, and the whole app is covered.
app.config.errorHandler = (error, _instance, info) => {
  console.error(`[app] ${info}`, error);
};

// Pinia before the router: the navigation guard reads the auth store.
app.use(createPinia());
app.use(router);

app.mount("#app");
