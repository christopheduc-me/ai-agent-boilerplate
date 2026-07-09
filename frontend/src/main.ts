import { createPinia } from "pinia";
import { createApp } from "vue";

import App from "@/App.vue";
import { router } from "@/router";

const app = createApp(App);
// Pinia before the router: the navigation guard reads the auth store.
app.use(createPinia());
app.use(router);

app.mount("#app");
