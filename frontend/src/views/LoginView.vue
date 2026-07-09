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
  <section>
    <h2>{{ mode === "login" ? "Log in" : "Create an account" }}</h2>
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
        {{ mode === "login" ? "Log in" : "Sign up" }}
      </button>
    </form>
    <p v-if="error" class="error">{{ error }}</p>
    <button type="button" class="link" @click="mode = mode === 'login' ? 'register' : 'login'">
      {{ mode === "login" ? "No account yet? Sign up" : "Already registered? Log in" }}
    </button>
  </section>
</template>

<style scoped>
form {
  display: grid;
  gap: 0.75rem;
  max-width: 320px;
}
label {
  display: grid;
  gap: 0.25rem;
}
.link {
  background: none;
  border: none;
  color: #0055cc;
  cursor: pointer;
  padding: 0;
  margin-top: 1rem;
}
</style>
