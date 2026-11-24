// UI smoke tests in the system Chrome (no browser download). The gateway is a STUB inside the test
// (page.routeWebSocket / page.route): nothing reaches a backend, and the dev server's proxy points at a dead port
// as a second guard. Never point these tests at a live AetherRV (prompt §5).
import { defineConfig } from "@playwright/test";

// the one Node global this file reads (the UI tsconfig carries browser types only)
declare const process: { env: Record<string, string | undefined> };

// UI_TEST_PORT lets parallel runs (one per page agent) use their own dev server and output folder
const PORT = Number(process.env.UI_TEST_PORT || 4317);
export default defineConfig({
  testDir: "tests/e2e",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  reporter: "list",
  outputDir: PORT === 4317 ? "test-results" : `test-results-${PORT}`,
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    channel: "chrome",
    viewport: { width: 1920, height: 1200 },
    trace: "off",
  },
  webServer: {
    command: `npx vite --port ${PORT} --strictPort`,
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: false,
    env: { AETHERRV_BACKEND: "http://127.0.0.1:9" },        // discard port: an unstubbed request can't reach anything
    stdout: "ignore",
  },
});
