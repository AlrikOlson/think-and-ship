import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Vite is started by `tauri dev`; the dev URL set here must match
// `build.devUrl` in crates/app-tauri/tauri.conf.json.
const port = 5173;

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      // Don't reload on Rust changes — Tauri rebuilds the backend.
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
  },
});
