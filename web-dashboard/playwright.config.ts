import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright config (M3, US-006).
 *
 * The smoke test runs against Vite's dev server (route-mocked, no real
 * gateway) so it can run anywhere npm + node are available without
 * needing a Rust toolchain or a running zeroclaw-gateway. CI for the
 * dashboard is wired separately in a follow-up — this config exists so
 * developers can `npm test` from `web-dashboard/` locally and on a
 * future GitHub Action.
 */
export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  workers: 1,
  reporter: "list",
  use: {
    baseURL: "http://localhost:5173",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  webServer: {
    command: "npm run dev -- --port=5173",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
});
