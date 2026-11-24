// AetherRV React UI. `npx vite` serves it on :5173 and proxies /ws and /api to a backend: by default the
// workstation-only snapshot server (harness/ui-reference/serve_snapshot.py, port 18080, no device controls).
// Point it elsewhere with AETHERRV_BACKEND=http://host:port (never at a live van while testing taps).
/// <reference types="vitest/config" />
import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "");
  const backend = env.AETHERRV_BACKEND || "http://127.0.0.1:18080";
  return {
    base: "/",
    plugins: [react()],
    build: {
      outDir: "dist",
      assetsDir: "assets",          // the gateway serves /assets/* immutable and / (index.html) no-cache; nothing else
      emptyOutDir: true,
      target: "es2022",
      sourcemap: false,
      assetsInlineLimit: 0,         // keep images as files under /assets/ (no data: URIs in the CSS/JS)
      chunkSizeWarningLimit: 600,   // three.js is one lazy chunk (~530 kB), loaded only for the 3D sky / bank
    },
    server: {
      host: "127.0.0.1",
      proxy: {
        "/ws": { target: backend, ws: true, changeOrigin: true },
        "/api": { target: backend, changeOrigin: true },
      },
    },
    preview: {
      host: "127.0.0.1",
      proxy: {
        "/ws": { target: backend, ws: true, changeOrigin: true },
        "/api": { target: backend, changeOrigin: true },
      },
    },
    test: {
      environment: "jsdom",
      include: ["src/**/*.test.{ts,tsx}"],
      restoreMocks: true,
    },
  };
});
