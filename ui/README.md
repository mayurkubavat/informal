# InFormal desktop app

A Tauri 2 app: a native window whose views are React, over the Rust core
linked in-process. UI v1 has the property manager, a Tcl console that
speaks the JasperGold or VC Formal command set, and a source view that
marks lines using the selected property's signals. Counterexample
waveforms (Surfer) come once the engines export traces.

```
ui/
  src/            React views (App.tsx) and the bridge to Rust (backend.ts)
  src-tauri/      the app crate: Tauri commands over informal_fpv::Worker
```

The app also serves its session to an outside AI agent on a local socket;
what the agent proposes waits in the Proposals tab for Accept or Reject
(`../docs/ai-interface.md`).

Tasks (decision D20) are nodes of the task tree: clicking one shows only
its properties, its status groups filter within it, and the current task is
marked. The property table's optional columns (engine, bound, time, CPU,
peak memory, lemmas, proof evidence, task) are shown or hidden from the
Columns button, a right-click on the header, or View > Table columns; the
choice is kept with the layout. The Design Info tab shows
`get_design_info -json`: the design's counts, the setup, and the current
task's model size; it is read when the tab opens and on Refresh. While a
prove runs, the progress readout names the running property, its engine,
the bound reached and its elapsed time. Every statistic is what the
engines measured (`../docs/fpv-shell.md`, Design and proof statistics).
The browser mock shows a second task with `?tasks=1`.

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
