import { defineConfig } from "vite";

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2022",
    // mermaid is a lazy-loaded chunk; its size doesn't affect startup.
    chunkSizeWarningLimit: 4000,
  },
});
