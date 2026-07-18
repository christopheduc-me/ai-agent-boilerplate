// Shared harness for view tests: a real memory-history router (the views call
// useRouter) and a fresh pinia. The api module is mocked per test file.
import { createPinia, setActivePinia, type Pinia } from "pinia";
import { createMemoryHistory, createRouter, type Router } from "vue-router";

export function makeRouter(): Router {
  const stub = { template: "<div />" };
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/login", name: "login", component: stub },
      { path: "/", name: "searches", component: stub },
    ],
  });
}

export function makePinia(): Pinia {
  const pinia = createPinia();
  setActivePinia(pinia);
  return pinia;
}
