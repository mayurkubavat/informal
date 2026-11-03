# Formal DV desktop app

A Tauri 2 app: a native window whose views are React, over the Rust core
linked in-process. UI v1 has the property manager, a Tcl console that
speaks the JasperGold or VC Formal command set, and a source view that
marks lines using the selected property's signals. Counterexample
waveforms (Surfer) come once the engines export traces.

```
ui/
  src/            React views (App.tsx) and the bridge to Rust (backend.ts)
  src-tauri/      the app crate: Tauri commands over fdv_fpv::Worker
```

## Run

```sh
cd ui
npm install
npm run dev          # views only, in a browser, against a mock backend
npm run tauri dev    # the desktop app (finds ../config/engines.toml and runs from the repo root)
npm run tauri dev -- -- $PWD/../tests/fpv/counter/counter.jg.tcl   # open with a script sourced
npm run tauri build  # .deb / AppImage on Linux, .app / .dmg on macOS
```

The desktop build needs, on Ubuntu:

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev librsvg2-dev
```

macOS needs only the Xcode command line tools. `src-tauri` is a separate
Cargo workspace, so `cargo build` at the repo root never needs these
packages.
