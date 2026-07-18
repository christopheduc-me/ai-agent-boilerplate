<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";

import { ApiError } from "@/api";
import { useAuthStore } from "@/stores/auth";

const auth = useAuthStore();
const router = useRouter();

const email = ref("");
const password = ref("");
const mode = ref<"login" | "register">("login");
const error = ref<string | null>(null);
const busy = ref(false);

async function submit(): Promise<void> {
  error.value = null;
  busy.value = true;
  try {
    if (mode.value === "register") {
      await auth.register(email.value, password.value);
    } else {
      await auth.login(email.value, password.value);
    }
    router.push({ name: "searches" });
  } catch (e) {
    error.value = e instanceof ApiError ? e.message : "unexpected error";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <section class="auth">
    <div class="card">
      <h2>{{ mode === "login" ? "Welcome back" : "Create an account" }}</h2>
      <p class="sub">
        {{
          mode === "login"
            ? "Log in to launch and follow your agent runs."
            : "Sign up to start running the agent."
        }}
      </p>
      <form @submit.prevent="submit">
        <label>
          Email
          <input v-model="email" type="email" required autocomplete="email" />
        </label>
        <label>
          Password
          <input
            v-model="password"
            type="password"
            required
            minlength="8"
            autocomplete="current-password"
          />
        </label>
        <button type="submit" :disabled="busy">
          {{ busy ? "…" : mode === "login" ? "Log in" : "Sign up" }}
        </button>
      </form>
      <p v-if="error" class="error">{{ error }}</p>
      <button type="button" class="link" @click="mode = mode === 'login' ? 'register' : 'login'">
        {{ mode === "login" ? "No account yet? Sign up" : "Already registered? Log in" }}
      </button>
    </div>
  </section>
</template>

<style scoped>
.auth {
  display: flex;
  justify-content: center;
  padding-top: 3rem;
}
.card {
  width: 100%;
  max-width: 380px;
  padding: 2rem;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
}
.card h2 {
  margin: 0 0 0.35rem;
  font-size: 1.4rem;
  letter-spacing: -0.02em;
}
.sub {
  margin: 0 0 1.5rem;
  color: var(--text-muted);
  font-size: 0.92rem;
}
form {
  display: grid;
  gap: 0.9rem;
}
label {
  display: grid;
  gap: 0.35rem;
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--text-muted);
}
form button {
  margin-top: 0.25rem;
}
.error {
  margin: 1rem 0 0;
  font-size: 0.9rem;
}
.link {
  display: block;
  width: 100%;
  margin-top: 1.25rem;
  padding: 0;
  background: none;
  border: none;
  color: var(--accent);
  font-weight: 500;
  text-align: center;
  cursor: pointer;
}
.link:hover {
  background: none;
  text-decoration: underline;
}
</style>
