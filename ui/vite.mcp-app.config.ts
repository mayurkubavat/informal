import { copyFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

// Builds a single self-contained SEP-1865 MCP App HTML document
// (`ui://informal/workbench.html`, `text/html;profile=mcp-app`) and writes it
// to `crates/informal-mcp/assets/workbench.html` for `informal mcp`.
export default defineConfig({
  plugins: [
    {
      name: "informal-mcp-app-flag",
      transformIndexHtml(html) {
        return html
          .replace('<link rel="icon" type="image/svg+xml" href="/logo.svg" />', "")
          .replace(
            "<head>",
            "<head><script>window.__INFORMAL_MCP_APP__ = true;</script>",
          );
      },
      closeBundle() {
        const src = resolve(__dirname, "dist-mcp-app/index.html");
        const dstDir = resolve(__dirname, "../crates/informal-mcp/assets");
        mkdirSync(dstDir, { recursive: true });
        copyFileSync(src, resolve(dstDir, "workbench.html"));
      },
    },
    react(),
    viteSingleFile(),
  ],
  clearScreen: false,
  build: {
    target: "es2021",
    outDir: "dist-mcp-app",
    assetsInlineLimit: 100_000_000,
    cssCodeSplit: false,
  },
});
