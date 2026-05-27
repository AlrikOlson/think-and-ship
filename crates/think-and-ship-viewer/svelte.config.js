import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  preprocess: vitePreprocess(),
  compilerOptions: {
    // Svelte 5 runes mode — `$state`, `$derived`, etc. instead of stores.
    runes: true,
  },
};
