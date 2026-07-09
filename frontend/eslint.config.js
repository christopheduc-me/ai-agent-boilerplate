import pluginVue from "eslint-plugin-vue";
import { defineConfigWithVueTs, vueTsConfigs } from "@vue/eslint-config-typescript";

export default defineConfigWithVueTs(
  { ignores: ["dist/", "node_modules/", "coverage/"] },
  pluginVue.configs["flat/essential"],
  vueTsConfigs.recommended,
);
