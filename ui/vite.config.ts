import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the dev build from this fixed port (see src-tauri/tauri.conf.json).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2021", outDir: "dist" },
});
