import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Unit and component tests only; Playwright owns tests/e2e and tests/visual.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test/setup.ts"],
    passWithNoTests: false,
    // The limits are for a busy machine: under CPU pressure the component
    // tests that type and click run several times slower, and the default
    // 5 s then fails tests that are only slow. A test that hangs still fails.
    testTimeout: 60_000,
    hookTimeout: 60_000,
  },
});
