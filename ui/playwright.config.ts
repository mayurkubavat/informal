import { defineConfig, devices } from "@playwright/test";

// Runs against the browser mock (`npm run dev`, src/backend.ts), not the
// Tauri app, so no Rust build is needed. Screenshots live under
// tests/__screenshots__/ and carry the platform in their names, so Linux and
// macOS baselines can sit side by side.
const size = { width: 1440, height: 900 };

export default defineConfig({
  testDir: "./tests",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: [["list"]],
  snapshotPathTemplate: "{testDir}/__screenshots__/{testFileName}/{arg}-{platform}{ext}",
  expect: {
    toHaveScreenshot: { maxDiffPixelRatio: 0.001, animations: "disabled", caret: "hide" },
  },
  use: {
    baseURL: "http://localhost:1420",
    trace: "off",
  },
  projects: [
    {
      name: "e2e",
      testMatch: /e2e\/.*\.spec\.ts/,
      use: { ...devices["Desktop Chrome"], viewport: size, deviceScaleFactor: 1 },
    },
    {
      // Sizes and colour schemes are set per describe block in the spec.
      name: "visual",
      testMatch: /visual\/.*\.spec\.ts/,
      use: { ...devices["Desktop Chrome"], viewport: size, deviceScaleFactor: 1 },
    },
  ],
  webServer: {
    command: "npm run dev",
    url: "http://localhost:1420",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
