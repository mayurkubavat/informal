// Browser mock for `npm run dev` and the Playwright suites: one counter
// design with six properties. `prove`/`check_fv` (all, or `-property`)
// run asynchronously like the desktop app: a "start" progress event, an
// "update" every tick (two properties at a time, k-bound rising), a console
// line per finished property, then "done" and the reply. `shell_stop` ends
// the run, leaving unfinished properties undetermined (reason "stopped").
// Under VC Formal `fvdisable`/`fvenable` toggle properties; any other Tcl
// answers with an error line.
//
// Query parameters (read once at load) steer it for the test suites:
//   ?fast=1      finish a prove in one go (no ticks; events still fire)
//   ?tick=<ms>   tick period, default 300
//   ?freeze=<n>  stop ticking after n ticks, leaving the run mid-way
//   ?holdStop=1  shell_stop never completes, so "Stopping…" stays up

import type { ConsoleLine, PropProgress, PropStatus, ProveProgress, Property, Reply, SessionView } from "./backend";

export const mockSource = `module counter (input logic clk, input logic rst_n, input logic en,
                output logic [3:0] q, output logic done);
    assign done = (q == 4'd9);
    always_ff @(posedge clk or negedge rst_n)
        if (!rst_n) q <= 4'd0;
        else if (en && !done) q <= q + 4'd1;
endmodule
`;

const mockSession: SessionView = {
  design: { files: ["tests/fpv/counter/counter.sv"], top: "counter", defines: [] },
  elaborated: true,
  clocks: [{ signal: "clk", edge: "posedge" }],
  reset: "!rst_n",
  props: [
    { name: "a_max", kind: "assert", expr: "q <= 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "a_step", kind: "assert", expr: "en && !done |=> q == $past(q) + 4'd1", enabled: true, status: { status: "unprocessed" } },
    { name: "a_never9", kind: "assert", expr: "q != 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "c_done", kind: "cover", expr: "$rose(done)", enabled: true, status: { status: "unprocessed" } },
    { name: "c_10", kind: "cover", expr: "q == 4'd10", enabled: true, status: { status: "unprocessed" } },
    { name: "m_en", kind: "assume", expr: "en |=> en", enabled: true, status: { status: "unprocessed" } },
  ],
  time_limit: { secs: 60, nanos: 0 },
  workdir: "fdv_work",
};

const mockResults: Record<string, PropStatus> = {
  a_max: { status: "proven", engine: "ric3-ic3", ms: 41 },
  a_step: { status: "proven", engine: "ric3", ms: 55 },
  a_never9: { status: "cex", engine: "ric3-bmc", depth: 11, ms: 30 },
  c_done: { status: "covered", engine: "ric3-bmc", depth: 11, ms: 31 },
  c_10: { status: "unreachable", engine: "ric3-ic3", ms: 29 },
};

// Ticks each property runs for before it resolves (default 2).
export const MOCK_TICKS: Record<string, number> = { a_max: 2, a_step: 3, a_never9: 2, c_done: 2, c_10: 1 };
// Reported times use a nominal tick, so frames are identical at any speed.
export const TICK_MS = 300;
const PARALLEL = 2;

export const mockOptions = { tick: 300, freeze: null as number | null, holdStop: false };
if (typeof location !== "undefined") {
  const q = new URLSearchParams(location.search);
  if (q.get("fast") === "1") mockOptions.tick = 0;
  if (q.has("tick")) mockOptions.tick = Math.max(0, Number(q.get("tick")) || 0);
  if (q.has("freeze")) mockOptions.freeze = Number(q.get("freeze"));
  if (q.get("holdStop") === "1") mockOptions.holdStop = true;
}

type Ev = "prove-progress" | "console-line";
const listeners: Record<Ev, Set<(p: unknown) => void>> = { "prove-progress": new Set(), "console-line": new Set() };
export const mockEvents = {
  on(ev: Ev, cb: (p: unknown) => void) {
    listeners[ev].add(cb);
    return () => void listeners[ev].delete(cb);
  },
  emit(ev: Ev, payload: ProveProgress | ConsoleLine) {
    for (const cb of [...listeners[ev]]) cb(payload);
  },
};

interface Run {
  props: Property[];
  prog: PropProgress[];
  ran: Map<string, number>;
  ticks: number;
  timer: ReturnType<typeof setTimeout> | null;
  resolve: (r: Reply) => void;
}
let active: Run | null = null;

const isFinal = (p: PropProgress) => p.status !== "running" && p.status !== "unprocessed";
const summaryOf = (props: Property[]) =>
  WORDS.map((w) => `${w} ${props.filter((p) => p.status.status === w).length}`).join(" ");
// Replace a progress entry's fields, keeping its name.
const reset = (p: PropProgress, to: Omit<PropProgress, "name">) => {
  for (const k of Object.keys(p)) if (k !== "name") delete (p as unknown as Record<string, unknown>)[k];
  Object.assign(p, to);
};

export function lineFor(name: string, s: PropStatus): string {
  const at = s.depth != null ? ` at depth ${s.depth}` : "";
  const why = s.reason ? ` (${s.reason})` : "";
  const how = s.engine ? ` [${s.engine}, ${s.ms ?? 0} ms]` : "";
  return `${name}: ${s.status}${at}${why}${how}`;
}

function emit(run: Run, kind: ProveProgress["kind"]) {
  mockEvents.emit("prove-progress", {
    kind,
    total: run.prog.length,
    done: run.prog.filter(isFinal).length,
    elapsed_ms: run.ticks * TICK_MS,
    props: run.prog.map((p) => ({ ...p })),
  });
}

// Start queued properties until PARALLEL are running.
function fill(run: Run) {
  for (const p of run.prog) {
    if (run.prog.filter((q) => q.status === "running").length >= PARALLEL) break;
    if (p.status !== "unprocessed") continue;
    reset(p, { status: "running", engine: mockResults[p.name]?.engine ?? "ric3", depth: 1, ms: 0 });
    run.ran.set(p.name, 0);
  }
}

function finish(run: Run, kind: "done" | "stopped") {
  if (run.timer) clearTimeout(run.timer);
  active = null;
  emit(run, kind);
  run.resolve({ ok: true, result: summaryOf(run.props), session: mockSession });
}

// One tick: advance running properties, resolve those that are due, start
// the next ones, report.
function step(run: Run) {
  run.ticks++;
  for (const p of run.prog) {
    if (p.status !== "running") continue;
    const n = (run.ran.get(p.name) ?? 0) + 1;
    run.ran.set(p.name, n);
    if (n >= (MOCK_TICKS[p.name] ?? 2)) {
      const final: PropStatus = mockResults[p.name] ?? { status: "undetermined", reason: "mock" };
      run.props.find((q) => q.name === p.name)!.status = { ...final };
      reset(p, final);
      mockEvents.emit("console-line", { kind: "out", text: lineFor(p.name, final) });
    } else Object.assign(p, { depth: 1 + n * 5, ms: n * TICK_MS });
  }
  fill(run);
  if (run.prog.every(isFinal)) finish(run, "done");
  else emit(run, "update");
}

function schedule(run: Run) {
  while (active === run && mockOptions.tick === 0 && !frozen(run)) step(run);
  if (active !== run || frozen(run)) return;
  run.timer = setTimeout(() => {
    run.timer = null;
    step(run);
    schedule(run);
  }, mockOptions.tick);
}
const frozen = (run: Run) => mockOptions.freeze !== null && run.ticks >= mockOptions.freeze;

function startProve(props: Property[]): Promise<Reply> {
  return new Promise((resolve) => {
    const run: Run = {
      props,
      prog: props.map((p) => ({ name: p.name, status: "unprocessed" })),
      ran: new Map(),
      ticks: 0,
      timer: null,
      resolve,
    };
    for (const p of props) p.status = { status: "unprocessed" };
    active = run;
    if (!props.length) return finish(run, "done");
    fill(run);
    emit(run, "start");
    schedule(run);
  });
}

function stopProve(run: Run) {
  let left = 0;
  for (const p of run.prog) {
    if (isFinal(p)) continue;
    left++;
    const s: PropStatus = { status: "undetermined", reason: "stopped", ...(p.engine ? { engine: p.engine } : {}), ...(p.ms !== undefined ? { ms: p.ms } : {}) };
    reset(p, s);
    run.props.find((q) => q.name === p.name)!.status = { ...s };
  }
  mockEvents.emit("console-line", { kind: "err", text: `prove stopped: ${left} propert${left === 1 ? "y" : "ies"} left undetermined` });
  finish(run, "stopped");
}

export const mockRunning = () => active !== null;

// Unit tests: drop any run in progress and return to the initial state.
export function resetMock() {
  if (active?.timer) clearTimeout(active.timer);
  active = null;
  mockFlavor = "jg";
  for (const p of mockSession.props) Object.assign(p, { enabled: true, status: { status: "unprocessed" } });
  Object.assign(mockOptions, { tick: 300, freeze: null, holdStop: false });
}

let mockFlavor = "jg";
const WORDS = ["proven", "cex", "covered", "unreachable", "undetermined", "error"] as const;

// Property names from `-property name`, `-property {a b}`, or a bare `{a b}`.
function names(script: string): string[] {
  const m = script.match(/-property\s+(?:\{([^}]*)\}|(\S+))/) ?? script.match(/^\s*\S+\s+(?:\{([^}]*)\}|(\S+))/);
  return m ? (m[1] ?? m[2]).split(/\s+/).filter(Boolean) : [];
}

export async function mock(cmd: string, args: Record<string, unknown>): Promise<Reply> {
  await new Promise((r) => setTimeout(r, 150));
  if (cmd === "shell_flavor") mockFlavor = String(args.flavor);
  const script = String(args.script ?? "");
  if (cmd === "shell_eval" && /^\s*(prove|check_fv)\b/.test(script)) {
    if (active) return { ok: false, result: "a prove is already running; stop it first", session: mockSession };
    const only = /-property/.test(script) ? names(script) : null;
    return startProve(mockSession.props.filter((p) => p.kind !== "assume" && p.enabled && (!only || only.includes(p.name))));
  }
  if (cmd === "shell_stop") {
    const run = active;
    if (!run) return { ok: true, result: "", session: mockSession };
    if (mockOptions.holdStop) return new Promise(() => {});
    stopProve(run);
    return { ok: true, result: "", session: mockSession };
  }
  // VC Formal only, as in crates/fdv-fpv/src/compat.rs.
  const toggle = script.match(/^\s*(fvdisable|fvenable)\b/);
  if (cmd === "shell_eval" && toggle && mockFlavor === "vcf") {
    const want = names(script);
    const hit = mockSession.props.filter((p) => want.includes(p.name));
    if (!hit.length) return { ok: false, result: `no property matches \`${want.join(" ")}\``, session: mockSession };
    for (const p of hit) p.enabled = toggle[1] === "fvenable";
    return { ok: true, result: "", session: mockSession };
  }
  if (cmd === "shell_eval" && script.trim() !== "") {
    return { ok: false, result: "browser preview: Tcl runs only in the desktop app", session: mockSession };
  }
  return { ok: true, result: "", session: mockSession };
}
