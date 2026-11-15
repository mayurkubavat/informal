import { defineConfig, devices } from "@playwright/test";

// Runs against the browser mock (`npm run dev`, src/backend.ts), not the
// Tauri app, so no Rust build is needed. Screenshots live under
// tests/__screenshots__/ and carry the platform in their names, so Linux and
// macOS baselines can sit side by side.
const size = { width: 1440, height: 900 };

// The suites start their own dev server on their own port and never reuse
// one that is already up: a server on the app's port 1420 may belong to
// another checkout, and the tests would then run against that tree. Two
// runs at once need different UI_TEST_PORT values (the second fails to
// start otherwise, which is the safe outcome).
const port = Number(process.env.UI_TEST_PORT ?? 1431);
const url = `http://localhost:${port}`;

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
    baseURL: url,
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
    command: `npx vite --port ${port} --strictPort`,
    url,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
