# Stop and live prove progress

How the workbench shows a prove while it runs and how Stop reaches the
engines. The frontend half is tested against the browser mock; the Rust half
is written but **has not been compiled yet** (see the end of this page).

## Event contract

Rust sends two Tauri events to the window. `ui/src/backend.ts` subscribes to
both: `backend.onProgress(cb)` and `backend.onConsole(cb)`. In a plain
browser it subscribes to `ui/src/mock.ts` instead.

### `prove-progress`

```ts
{
  kind: "start" | "update" | "done" | "stopped",
  total: number,       // properties in this run
  done: number,        // of them, finished (any final status)
  elapsed_ms: number,  // since the run started
  props: [{ name, kind, status, engine?, depth?, ms?, reason?, message? }]
}
```

`kind` is `"assert"` or `"cover"` (assumes are never in a run). It lets the UI
list a property that its last session does not have yet; see "Rows the
session does not know yet" below. The UI treats it as optional and guesses
from the name prefix if an older backend leaves it out.

- Every event carries **all** properties in the run, in run order, so the
  UI can redraw from any single event. Properties outside the run are not
  listed and keep the session's status.
- Per property, `status` is:
  - `"unprocessed"` while queued;
  - `"running"` while engines work on it. `depth` is the deepest k-bound an
    engine has reported (0 until one does), `engine` is the engine that
    reported it, and `ms` is the time spent on the property so far;
  - a final status word once it finishes (`proven`, `cex`, `covered`,
    `unreachable`, `undetermined`, `error`), with the same fields as the
    session's `PropStatus`.
- `start` comes once, when the targets are known. `update` comes each time a
  property starts, reaches a deeper bound or finishes, plus a heartbeat every
  500 ms so elapsed times keep moving. The run ends with exactly one `done`,
  or one `stopped` if Stop ended it. In a stopped run, every property that
  had not finished is `undetermined` with `reason: "stopped"`.
- The `shell_eval` / `shell_source` call that started the prove returns
  **after** the final event, with the session. From then on the UI draws
  from the session again.

### `console-line`

```ts
{ text: string, kind: "out" | "err" }
```

There is one line per finished property, for example
`a_never9: cex at depth 11 [ric3-bmc, 30 ms]`. A stopped run adds
`prove stopped: N properties left undetermined` (kind `err`). The UI adds
each line to the console under the echoed command.

### `engine-log`

`{ prop, engine, text, err }`: one line of an external engine's own output
while it works on `prop` (`err` = stderr). fdv-engines-ext reads the
engine's pipes line by line and forwards each line through the run's
progress channel (`fdv_engine::Progress::output`), up to 2,000 lines per
stream per engine run, then one "further output not shown" line. Lines
arrive as the engine flushes them: Rust engines (rIC3) flush per line; C
programs that block-buffer a pipe (ABC) deliver theirs when they exit. Only
sessions with a progress sink (the app) pay for this; `fdv bench` and the
CLI pass no channel. In-process engines (fdv-bmc, fdv-ic3, ...) print
nothing. The UI shows these in the bottom dock's Log tab (Ctrl+4), capped
at 5,000 lines.

### `shell_complete`

`shell_complete(prefix) -> string[]`: the active flavor's command names
(fdv_fpv::compat::command_names), then property names, starting with
`prefix`. Answered from the app's cache of the last reply, so it works
while a prove runs. The console's Tab inserts a single match with a space;
several insert the first and list them under the prompt, and Tab /
Shift+Tab cycle through them.

### `shell_stop`

The command returns `{ ok: true, result: "", session: null }` at once. It
does not go through the shell thread, because that thread is busy with the
prove. It sets the session's stop flag. The UI shows "Stopping…" until the
`stopped` (or `done`) event arrives.

## UI behaviour (ui/src/App.tsx)

- "running" is a view status (`model.ts`: `ViewStatus`, `withProgress`), not
  a backend `StatusWord`. While a run is live, its progress is laid over the
  session's properties:
  - rows show the turning accent ring and "Running";
  - Bound shows `k=<depth>` and Time shows `m:ss`;
  - the task tree has a Running group.
- In the tool row, Prove all is disabled and Stop is enabled (its tooltip
  names Esc, like the other tool buttons). At the right end sit a 120 px
  progress bar and "`done` of `total` · `m:ss`" in a fixed-width,
  tabular-figure box, so the readout ticking never moves the tools. The tool
  row does not wrap or squeeze its buttons; under 1000 px only the bar is
  shown. The status bar shows "Proving · done/total".
- Rows the session does not know yet: a `source` of a script that adds
  properties and proves them in the same call gets progress events naming
  properties the UI's session (from before the call) does not have.
  `withProgress` (`model.ts`) appends those as provisional rows (name in
  italics, kind from the event) after the session's own, and keeps them
  until the script's reply brings the session, which replaces them. The
  mock reproduces this with `source mock/prove-on-source.tcl`.
  `stop-progress-live.png` is the desktop app mid-run in that case (a
  start-up script that adds seven properties and runs `prove -all`, with a
  fake engine that sleeps 4 s per property), in a window under 1000 CSS px,
  so the progress bar shows without its readout.
- Esc stops the prove, unless a menu is open or the filter uses the key
  first. Prove > Stop does the same.
- Only Prove actions (and Flavor, Run script, Disable/Enable) are blocked
  during a prove. The console stays usable. A second prove, whether from
  the button, F5, the context menu or typed, is refused in the UI with a
  console error. The mock refuses one too.

## Rust side (written, not compiled)

| File | Change |
| --- | --- |
| `crates/fdv-engine/src/portfolio.rs` | `run_until(engines, req, progress, stop: Option<&AtomicBool>)`. A watcher thread copies the caller's flag into the run's own `CancelToken` every 20 ms. `run` calls it with `None`. Taking a winner cancels the run's token, not the caller's flag. New test `a_stop_flag_cancels_the_run_and_is_left_alone`. |
| `crates/fdv-fpv/src/progress.rs` (new) | `ProveProgress` / `PropProgress` / `ProgressKind`: the payload above, serde-serialised. `ProveSink` trait (`progress`, `line`). `Tracker` holds a run's state and emits events. Two unit tests. |
| `crates/fdv-fpv/src/session.rs` | Two `#[serde(skip)]` fields: `stop: Arc<AtomicBool>` and `sink: Option<Arc<dyn ProveSink>>`. `clear()` keeps both. `prove()` clears `stop`, starts a `Tracker` when there is a sink, runs a 500 ms heartbeat thread, and runs the per-property loop (now `prove_targets`). The loop checks `stop` before each property and passes it to `portfolio::run_until`. When it halts, the running property and every property not reached become `Undetermined { reason: "stopped" }`. `check_one` forwards engine `Progress` (bound) to the tracker. |
| `crates/fdv-fpv/src/worker.rs` | `Worker` holds the session's stop flag. `Worker::stop()` sets it from any thread without queueing. The test also calls `stop()` while idle. |
| `crates/fdv-fpv/src/lib.rs` | `pub mod progress` and re-exports. |
| `ui/src-tauri/src/main.rs` | `EventSink` (a `ProveSink`) emits `prove-progress` / `console-line` through `AppHandle::emit` (`use tauri::Emitter`). The `AppHandle` is stored in a `OnceLock` in `setup`. New `shell_stop` command, registered in `generate_handler!`. The session gets the sink in the worker's `make` closure. |

How the engines actually stop: external engines (`fdv-engines-ext`) already
poll `ctx.should_stop()` every 10 ms. When it turns true they kill their
process group (`killpg SIGKILL`), so rIC3 workers and grandchildren go with
them. In-process engines (`fdv-bmc`) already check `should_stop()` in their
loops. Stop therefore needs no change in either crate: it only has to reach
the run's `CancelToken`, which `run_until` does.

### Known limitations

- **Elaboration and model building cannot be interrupted.** slang lowering
  and Yosys runs are single blocking calls. A Stop during them takes effect
  when the first engine starts: the portfolio is cancelled at once, and
  that property and the rest become "stopped". A Stop during
  `elaborate`, which runs before the targets are known, has the same delay.
- **Console commands typed during a prove wait.** The shell thread runs one
  request at a time, so a console command sent while a prove runs gets its
  reply after the prove ends. The UI keeps the input enabled (the mock
  answers at once). Running Tcl alongside a prove would need a second
  interpreter or a reentrant event loop, which is out of scope.
- **Stop just before a prove starts is ignored.** `prove()` clears the flag
  when it starts, so a Stop sent between clicking Prove and the shell
  thread picking up the request is lost. The UI enables Stop as soon as
  the prove request is sent, so a very fast click can land in that window.
  The prove then runs to the end, and "Stopping…" clears when its call
  returns, never stuck. Clearing the flag at the end of a prove instead
  would move the race to the other end: a late Stop would cancel the next
  prove. Closing it fully needs a prove id passed with `shell_stop`.
- `depth` while running is whatever engines report through
  `RunContext::report`. Engines that never report leave it at 0 (`k=0`).

## To verify once Rust may be compiled

1. `cargo test -p fdv-engine -p fdv-fpv`. This covers the new portfolio
   test, the progress tests and the worker test.
2. `cargo clippy -p fdv-fpv -p fdv-engine`, then `cd ui && npm run tauri dev`
   (builds `fdv-app`).
3. In the app, run `source tests/fpv/counter/counter.jg.tcl` and then
   Prove all. Check that rows go Running, Bound counts up where engines
   report bounds, the progress bar and status bar advance, and console
   lines appear per property.
4. Stop during a long property (for example a hard HWMCC model with a
   long time limit):
   - "Stopping…" should show briefly, then the property and the rest read
     "Undetermined · stopped";
   - Prove all comes back;
   - `pgrep -f ric3` (or the engine binary) should show no leftover
     processes.
5. Press Esc during a prove (stops it) and with the Prove menu open (only
   closes the menu).
6. Type a command in the console during a prove. Its reply should appear
   after the prove; that is expected (see the limitations above).
7. Check that the `fdv` CLI's `prove` still behaves the same: no sink, and
   the stop flag is never set.
