// Calls into the Rust core. Inside the desktop app this goes through Tauri
// commands (src-tauri/src/main.rs); in a plain browser (`npm run dev`) a
// small mock stands in so the views can be worked on without the app.

import { mock, mockEvents, mockSource } from "./mock";

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
// with reason "stopped".
export type ProgressKind = "start" | "update" | "done" | "stopped";
export interface PropProgress {
  name: string;
  status: StatusWord | "running";
  engine?: string;
  depth?: number | null;
  ms?: number;
  reason?: string;
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
  readText: async (path: string): Promise<string> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<string>("read_text", { path });
    }
    return mockSource;
  },
};
