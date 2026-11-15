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
  },
});
