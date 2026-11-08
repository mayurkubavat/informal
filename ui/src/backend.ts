// Calls into the Rust core. Inside the desktop app this goes through Tauri
// commands (src-tauri/src/main.rs); in a plain browser (`npm run dev`) a
// small mock stands in so the views can be worked on without the app.

import { mock, mockEvents, mockText, mockTrace } from "./mock";

export type StatusWord =
  | "unprocessed" | "proven" | "cex" | "covered" | "unreachable" | "undetermined" | "error";

export interface PropStatus {
  status: StatusWord;
  engine?: string;
  depth?: number | null;
  ms?: number;
  reason?: string;
  message?: string;
}

export interface Property {
  name: string;
  kind: "assert" | "assume" | "cover";
  expr: string;
  enabled: boolean;
  status: PropStatus;
  // Path of the VCD written for a CEX or cover (inside the session's
  // workdir, e.g. "fdv_work/a_never9.vcd"); absent or null when there is
  // none. See docs/ui/trace-format.md.
  trace?: string | null;
  // Where the property was written: the script line of the Tcl command that
  // created it (absolute path, 1-based line), or an RTL file:line for SVA
  // in the design. Absent or null for properties typed at the console; the
  // UI then falls back to searching the top file (model.ts locate).
  source?: SourceLoc | null;
}

export interface SourceLoc {
  file: string;
  line: number;
}

export interface SessionView {
  design: { files: string[]; top: string | null; defines: [string, string | null][] };
  elaborated: boolean;
  clocks: { signal: string; edge: string }[];
  reset: string | null;
  props: Property[];
  time_limit: { secs: number; nanos: number };
  workdir: string;
}

export interface Reply {
  ok: boolean;
  result: string;
  session: SessionView | null;
}

export type Flavor = "jg" | "vcf";

// Live prove progress, pushed by the backend while a prove runs (Tauri event
// "prove-progress"; see docs/ui/stop-progress.md). Every event carries the
// full list of properties in the run, so a listener can redraw from any one
// of them. Per property, `status` is "unprocessed" while queued, "running"
// while an engine works on it (depth = current k-bound, ms = time so far),
// then its final word. A stopped run reports leftovers as "undetermined"
// with reason "stopped". Each entry also names the property's kind.
export type ProgressKind = "start" | "update" | "done" | "stopped";
export interface PropProgress {
  name: string;
  // Sent by current backends; lets the UI list a property the session does
  // not have yet. Optional so an older backend's events still parse.
  kind?: "assert" | "cover";
  status: StatusWord | "running";
  engine?: string;
  depth?: number | null;
  ms?: number;
  reason?: string;
  message?: string;
}
export interface ProveProgress {
  kind: ProgressKind;
  total: number;
  done: number;
  elapsed_ms: number;
  props: PropProgress[];
}
// One line of engine output for the console (Tauri event "console-line").
export interface ConsoleLine {
  text: string;
  kind: "out" | "err";
}

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call(cmd: string, args: Record<string, unknown> = {}): Promise<Reply> {
  if (inTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<Reply>(cmd, args);
  }
  return mock(cmd, args);
}

// Subscribe to a backend event; returns an unsubscribe function that is safe
// to call before the Tauri listener has finished registering.
function subscribe<T>(event: "prove-progress" | "console-line", cb: (payload: T) => void): () => void {
  if (!inTauri) return mockEvents.on(event, cb as (p: unknown) => void);
  let off: (() => void) | null = null;
  let dead = false;
  import("@tauri-apps/api/event")
    .then(({ listen }) => listen<T>(event, (e) => cb(e.payload)))
    .then((un) => (dead ? un() : (off = un)));
  return () => {
    dead = true;
    off?.();
  };
}

export const backend = {
  inTauri,
  snapshot: () => call("snapshot"),
  evalTcl: (script: string) => call("shell_eval", { script }),
  source: (path: string) => call("shell_source", { path }),
  setFlavor: (flavor: Flavor) => call("shell_flavor", { flavor }),
  // Ask a running prove to stop. The prove's own call then resolves with the
  // session; a "stopped" progress event marks the moment it took effect.
  stop: () => call("shell_stop"),
  onProgress: (cb: (p: ProveProgress) => void) => subscribe("prove-progress", cb),
  onConsole: (cb: (l: ConsoleLine) => void) => subscribe("console-line", cb),
  // A design file or script for the source view. The app only reads files
  // under its working directory (the repo) or in the design's file list.
  readText: async (path: string): Promise<string> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<string>("read_text", { path });
    }
    const t = mockText(path);
    if (t === null) throw new Error(`${path}: no such file`);
    return t;
  },
  // The VCD text of a property's trace. The app only reads files inside the
  // session's workdir; the mock serves tests/fixtures/a_never9.vcd.
  readTrace: async (path: string): Promise<string> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<string>("read_trace", { path });
    }
    const t = mockTrace(path);
    if (t === null) throw new Error(`${path}: no such trace`);
    return t;
  },
};
