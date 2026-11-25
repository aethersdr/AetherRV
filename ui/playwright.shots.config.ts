// README screenshots (harness/ui-reference/screenshots.sh runs this): the production bundle, served by `vite preview`,
// fed a sanitized state snapshot and a copy of the history database by a STUB gateway (shots/readme.spec.ts). Nothing
// reaches a backend: the preview's proxy points at a dead port, and the stub drops every command.
import { defineConfig } from "@playwright/test";

const PORT = 4398;
export default defineConfig({
  testDir: "shots",
  timeout: 180_000,
  workers: 1,
  reporter: "list",
  outputDir: "test-results-shots",
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    channel: "chrome",
    viewport: { width: 1920, height: 1200 },
    deviceScaleFactor: 1,
    timezoneId: "America/Denver",          // the README's stand-in place (the sanitized position)
    locale: "en-US",
  },
  webServer: {
    command: `npx vite build && npx vite preview --port ${PORT} --strictPort`,
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: false,
    timeout: 180_000,
    env: { AETHERRV_BACKEND: "http://127.0.0.1:9" },        // discard port: an unstubbed request can't reach anything
    stdout: "ignore",
  },
});
