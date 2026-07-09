<script setup lang="ts">
import { useRouter } from "vue-router";

import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

function logout(): void {
  auth.logout();
  router.push({ name: "login" });
}
</script>

<template>
  <header class="topbar">
    <h1>AI Agent Boilerplate</h1>
    <button v-if="auth.isAuthenticated" type="button" @click="logout">Log out</button>
  </header>
  <main>
    <RouterView />
  </main>
</template>

<style>
body {
  font-family: system-ui, sans-serif;
  margin: 0;
  color: #1a1a1a;
}
main {
  max-width: 720px;
  margin: 0 auto;
  padding: 1rem;
}
.topbar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 0.5rem 1rem;
  border-bottom: 1px solid #ddd;
}
.topbar h1 {
  font-size: 1.1rem;
  margin: 0;
}
.error {
  color: #b00020;
}
</style>
