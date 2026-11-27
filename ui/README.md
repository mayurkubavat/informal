# InFormal Desktop Workbench (`ui/`)

A cross-platform **Tauri 2 + React** desktop workbench over the InFormal Rust core linked in-process, plus an embedded single-file **MCP App** bundle (`vite.mcp-app.config.ts`) rendered directly inside MCP-App-capable IDEs.

## Features

- **Property Manager & Multi-Task Tree:** Task-scoped filtering (`<embedded>` and user tasks), optional proof-telemetry columns (engine, bound, time, CPU, peak memory, lemmas, proof evidence, task), and JasperGold/VC Formal context menus.
- **3-Pane CEX Debug Workspace:** Synchronized **CodeMirror 6 RTL SourceView** (inline `@C<k>` live trace values, `SVA-FPV` / `SV-RTL` lint quick-fixes, `signal_nav` go-to-driver/load), **HiDPI `<canvas>` TraceView** (streamed FST/VCD traces, `Why CEX` dynamic backward causal slice, VCD/FST export, Surfer launch), and **ELK.js COI & Causal SchematicView**.
- **Eight Built-In Formal Apps Dock:** Interactive tabs for `Coverage`, `X-Prop`, `SPV`, `SEC / LEC`, `CSR / Conn`, `Bus VIP`, and `Low Power` (IEEE 1801 UPF).
- **Keyboard-First UX:** Four workspace presets (`Triage`, `CEX Debug`, `Split`, `Signoff`), Light/Dark/System themes, Command Palette (`Cmd/Ctrl+K`), Shortcut Sheet (`?`), and modal focus traps.
- **AI Proposals, Walkthroughs & Ask Mode:** Serves the live session over a local Unix socket (`informal mcp --connect`), renders human-reviewed Accept/Reject Proposal cards with `[AI-added]` / `[under AI-added]` provenance, supports guided multi-cycle `WalkthroughBar` tours, and includes a read-only Ask console (`../docs/ui/ask-mode.md`).

```text
ui/
  src/             React workbench views, Zustand store, HiDPI canvas trace, ELK schematic, MCP App entry
  src-tauri/       Tauri 2 Rust bridge over informal_fpv::Worker and local MCP socket
  tests/e2e/       Playwright UI, WebKit 2x HiDPI, MCP iframe, fuzz, and real-backend Layer-5 E2E tests
```

## Run & Build

```sh
cd ui
npm ci

npm run dev              # browser preview against mock backend (or ?bridge=http://... for real backend)
npm run tauri dev        # live desktop app (locates ../config/engines.toml from repo root)
npm run tauri dev -- -- "$PWD/../examples/02-fpv-fundamentals/fifo/run.jg.tcl"
CARGO_BUILD_JOBS=2 npx tauri build --bundles app   # macOS .app / Linux .deb / AppImage
```

On Ubuntu/Debian, install WebKitGTK & Tauri system dependencies first:

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

## Test

```sh
cd ui
npx tsc --noEmit
npm test                 # Vitest unit, store, interaction & fuzz tests
npm run test:e2e         # Playwright E2E, real-backend Layer 5, and WebKit 2x HiDPI checks
```

See [Desktop Workbench Guide](../docs/guide/workbench-ui.md) and [UI Testing Notes](../docs/ui/testing.md) for full details.
