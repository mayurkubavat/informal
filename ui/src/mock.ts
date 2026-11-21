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
//   ?props=<n>   add n generated properties (p_00000 ...) after the six,
//                for the virtualized table tests
//   ?design=fifo show tests/fpv/fifo's scope tree (two ptr instances) in
//                the Design tab instead of the counter's
//   ?surfer=1    report the Surfer viewer as installed (tool_available);
//                "Open in Surfer" then succeeds without opening anything
//   ?llm=offline start with the LLM provider unreachable (Ask disabled)
//   ?ai=1        an agent has proposed two changes through the AI interface
//                (an assumption and a helper assertion), both pending
//
// Ask mode: a Fake provider ("fake/scripted") streams a scripted answer a
// word at a time. A question about failures is answered from the session
// with no command; anything else gets `prove -property a_never9`. The
// second provider, "ollama-gemma", is always offline in the browser.
//   ?design=scan no hierarchy (no SystemVerilog front end): the Design tab
//                falls back to scanning the top file's ports
//
// `source mock/prove-on-source.tcl` stands for a script that adds two
// properties and proves them in the same call: progress events name them
// before the session has them, which joins them on the reply.

import counterTrace from "../tests/fixtures/a_never9.vcd?raw";
import type { AiProposal, AiStatus, ConsoleLine, DesignInfo, DesignScope, FormalLintFindingView, FormalQuickFixView, GuiAction, GuiRequest, GuiState, EngineLine, EngineRun, PropProgress, PropStatus, ProofStats, ProveProgress, Property, Reply, SchematicGraphView, SessionView, SignalNavView, WhyCexReport } from "./backend";
import { splitAnswer, type AskDelta, type AskDone, type LlmProviderEntry, type LlmStatus } from "./ask";
import { encodeVcdToIftr, generateSyntheticIftr } from "./traceBin";
import { parseVcd } from "./vcd";

export const mockSource = `module counter (input logic clk, input logic rst_n, input logic en,
                output logic [3:0] q, output logic done);
    assign done = (q == 4'd9);
    always_ff @(posedge clk or negedge rst_n)
        if (!rst_n) q <= 4'd0;
        else if (en && !done) q <= q + 4'd1;
    m_en: assume property (@(posedge clk) en |=> en);
endmodule
`;

// The script the mock's properties came from (Property.source points here,
// except m_en, which is SVA in the RTL above).
export const MOCK_SCRIPT = "tests/fpv/counter/counter.jg.tcl";
export const MOCK_RTL = "tests/fpv/counter/counter.sv";
export const mockScript = `# JasperGold-style FPV setup for the counter.
clear -all
analyze -sv12 counter.sv
elaborate -top counter
clock clk
reset -expression {!rst_n}

assert -name a_max    {q <= 4'd9}
assert -name a_step   {en && !done |=> q == $past(q) + 4'd1}
assert -name a_never9 {q != 4'd9}
cover  -name c_done   {$rose(done)}
cover  -name c_10     {q == 4'd10}

prove -all
`;
let mockRtlOverride: string | null = null;
export function mockText(path: string): string | null {
  if (path === MOCK_RTL) return mockRtlOverride ?? mockSource;
  if (path === MOCK_SCRIPT) return mockScript;
  return null;
}
const at = (file: string, line: number) => ({ file, line });

// the scope tree for the counter above (informal_svlang::Scope, as the app's
// snapshot carries it in design.hierarchy).
const port = (name: string, dir: "input" | "output", width = 1) => ({ name, width, kind: "port" as const, dir });
export const MOCK_HIERARCHY: DesignScope[] = [
  {
    path: "counter", name: "counter", module: "counter", children: [],
    signals: [port("clk", "input"), port("rst_n", "input"), port("en", "input"), port("q", "output", 4), port("done", "output")],
  },
];
// tests/fpv/fifo/fifo.sv elaborated with its defaults (DEPTH_LOG2 = 2, DW = 8).
const ptr = (name: string): DesignScope => ({
  path: `fifo.${name}`, name, module: "ptr", children: [],
  signals: [port("clk", "input"), port("rst_n", "input"), port("inc", "input"), port("p", "output", 2), { name: "W", width: 32, kind: "parameter" }],
});
export const MOCK_FIFO: DesignScope[] = [
  {
    path: "fifo", name: "fifo", module: "fifo", children: [ptr("u_wp"), ptr("u_rp")],
    signals: [
      port("clk", "input"), port("rst_n", "input"), port("push", "input"), port("pop", "input"), port("din", "input", 8),
      port("dout", "output", 8), port("full", "output"), port("empty", "output"), port("status", "output", 2),
      { name: "wp", width: 2, kind: "variable" }, { name: "rp", width: 2, kind: "variable" }, { name: "count", width: 3, kind: "variable" },
      { name: "mem", width: 32, kind: "variable" }, { name: "do_push", width: 1, kind: "variable" }, { name: "do_pop", width: 1, kind: "variable" },
      { name: "DEPTH_LOG2", width: 32, kind: "parameter" }, { name: "DW", width: 32, kind: "parameter" }, { name: "DEPTH", width: 32, kind: "parameter" },
    ],
  },
];

const mockSession: SessionView = {
  design: { files: [MOCK_RTL], top: "counter", defines: [], hierarchy: MOCK_HIERARCHY },
  elaborated: true,
  clocks: [{ signal: "clk", edge: "posedge" }],
  reset: "!rst_n",
  props: [
    { name: "a_max", source: at(MOCK_SCRIPT, 8), kind: "assert", expr: "q <= 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "a_step", source: at(MOCK_SCRIPT, 9), kind: "assert", expr: "en && !done |=> q == $past(q) + 4'd1", enabled: true, status: { status: "unprocessed" } },
    { name: "a_never9", source: at(MOCK_SCRIPT, 10), kind: "assert", expr: "q != 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "c_done", source: at(MOCK_SCRIPT, 11), kind: "cover", expr: "$rose(done)", enabled: true, status: { status: "unprocessed" } },
    { name: "c_10", source: at(MOCK_SCRIPT, 12), kind: "cover", expr: "q == 4'd10", enabled: true, status: { status: "unprocessed" } },
    { name: "m_en", source: at(MOCK_RTL, 7), kind: "assume", expr: "en |=> en", enabled: true, status: { status: "unprocessed" } },
  ],
  time_limit: { secs: 60, nanos: 0 },
  workdir: "informal_work",
};

const mockResults: Record<string, PropStatus> = {
  a_max: { status: "proven", engine: "ric3-ic3", ms: 41 },
  a_step: { status: "proven", engine: "ric3", ms: 55 },
  a_never9: { status: "cex", engine: "ric3-bmc", depth: 11, ms: 30 },
  c_done: { status: "covered", engine: "ric3-bmc", depth: 11, ms: 31 },
  c_10: { status: "unreachable", engine: "ric3-ic3", ms: 29 },
  a_src_ok: { status: "proven", engine: "ric3-ic3", ms: 44 },
  c_src_hit: { status: "covered", engine: "ric3-bmc", depth: 4, ms: 22 },
};

// What each result's prove measured (stats.rs ProofStats), as the counter's
// real run reports it.
export const MOCK_STATS: Record<string, ProofStats> = {
  a_max: { engine: "ric3-ic3", wall_ms: 41, cpu_ms: 101, depth: 3, depth_kind: "frames", max_bound: 24, invariant_clauses: 6, evidence: "invariant", peak_rss_kb: 38800 },
  a_step: { engine: "ric3", wall_ms: 55, cpu_ms: 74, max_bound: 31 },
  a_never9: { engine: "ric3-bmc", wall_ms: 30, cpu_ms: 87, depth: 11, depth_kind: "trace", max_bound: 11, evidence: "trace", peak_rss_kb: 42112 },
  c_done: { engine: "ric3-bmc", wall_ms: 31, cpu_ms: 78, depth: 11, depth_kind: "trace", max_bound: 11, evidence: "trace" },
  c_10: { engine: "ric3-ic3", wall_ms: 29, cpu_ms: 91, depth: 5, depth_kind: "frames", max_bound: 20, invariant_clauses: 3, evidence: "invariant", peak_rss_kb: 45312 },
};

// `get_design_info -json` for the counter (its real counts).
export function mockDesignInfo(): DesignInfo {
  const props = mockSession.props.filter((p) => (p.task ?? "<embedded>") === (mockSession.current_task ?? "<embedded>"));
  return {
    top: "counter",
    files: 1,
    elaborated: true,
    design: {
      instances: 1, modules: 1, inputs: 3, input_bits: 3, outputs: 2, output_bits: 5, flops: 1, flop_bits: 4, latches: 0, latch_bits: 0,
      memories: [], clocks: [{ signal: "clk", edges: ["posedge"], registers: 1 }], resets: [{ signal: "rst_n", active_high: false, registers: 1 }], undriven: 0,
    },
    declared_clocks: ["clk"],
    declared_reset: "!rst_n",
    black_boxes: [],
    task: mockSession.current_task ?? "<embedded>",
    cut_points: mockSession.tasks?.find((t) => t.name === mockSession.current_task)?.stopats ?? [],
    properties: {
      asserts: props.filter((p) => p.kind === "assert").length,
      covers: props.filter((p) => p.kind === "cover").length,
      assumptions: props.filter((p) => p.kind === "assume" && p.enabled).length,
      disabled: props.filter((p) => !p.enabled).length,
    },
    model: { nodes: 81, states: 7, state_bits: 14, array_states: 0, inputs: 3, input_bits: 3, constraints: 2, bad: 6, justice: 0, aig: { inputs: 3, latches: 14, ands: 84 } },
  };
}

// The portfolio's per-engine record for each result (the proof profiler):
// the winner first as in mockResults, the rest cancelled when it won.
const run = (engine: string, outcome: EngineRun["outcome"], ms: number, depth?: number): EngineRun =>
  depth === undefined ? { engine, outcome, ms } : { engine, outcome, ms, depth };
export const MOCK_ENGINES: Record<string, EngineRun[]> = {
  a_max: [run("ric3-ic3", "won", 41, 3), run("ric3-bmc", "killed", 42, 24), run("informal-kind", "killed", 43, 6)],
  a_step: [run("ric3-ic3", "killed", 56, 4), run("ric3-bmc", "killed", 56, 31), run("ric3", "won", 55, 2)],
  a_never9: [run("ric3-ic3", "killed", 31, 2), run("ric3-bmc", "won", 30, 11), run("informal-kind", "killed", 31, 9)],
  c_done: [run("ric3-ic3", "killed", 32, 2), run("ric3-bmc", "won", 31, 11), run("informal-kind", "error", 2)],
  c_10: [run("ric3-ic3", "won", 29, 5), run("ric3-bmc", "timeout", 29, 20), run("informal-kind", "lost", 12, 4)],
  a_src_ok: [run("ric3-ic3", "won", 44, 3), run("ric3-bmc", "killed", 45, 26)],
  c_src_hit: [run("ric3-ic3", "killed", 23, 1), run("ric3-bmc", "won", 22, 4)],
};

// Properties whose result comes with a trace: both get the counter's 12
// cycle fixture (q reaches 9, so done rises, in cycle 11).
export const MOCK_TRACES: Record<string, string> = { a_never9: "informal_work/a_never9.vcd", c_done: "informal_work/c_done.vcd" };
// The app's open_in_surfer: refuses when Surfer is missing or the path is
// not one of the traces; otherwise records the path (for the tests).
export const mockSurferOpened: string[] = [];
export async function mockOpenInSurfer(path: string): Promise<void> {
  if (!mockOptions.surfer) throw new Error("surfer is not on PATH (install it from surfer-project.org)");
  if (mockTrace(path) === null) throw new Error(`${path}: not a trace in informal_work`);
  mockSurferOpened.push(path);
}
export function mockTrace(path: string): string | null {
  return Object.values(MOCK_TRACES).includes(path) ? counterTrace : null;
}

// What `source mock/prove-on-source.tcl` adds (and then proves).
export const PROVE_ON_SOURCE = "mock/prove-on-source.tcl";
const sourcedProps = (): Property[] => [
  { name: "a_src_ok", kind: "assert", expr: "done |-> q == 4'd9", enabled: true, status: { status: "unprocessed" } },
  { name: "c_src_hit", kind: "cover", expr: "q == 4'd4", enabled: true, status: { status: "unprocessed" } },
];
const BASE_PROPS = mockSession.props.length;

// `?props=n`: n extra properties, mostly asserts with a cover every 7th, all
// unprocessed (a prove resolves them to undetermined).
export function generatedProps(n: number): Property[] {
  const w = String(Math.max(0, n - 1)).length;
  return Array.from({ length: n }, (_, i) => ({
    name: `p_${String(i).padStart(Math.max(5, w), "0")}`,
    kind: i % 7 === 6 ? ("cover" as const) : ("assert" as const),
    expr: `q != 4'd${i % 16}`,
    enabled: true,
    status: { status: "unprocessed" as const },
  }));
}

// Ticks each property runs for before it resolves (default 2).
export const MOCK_TICKS: Record<string, number> = { a_max: 2, a_step: 3, a_never9: 2, c_done: 2, c_10: 1 };
// Reported times use a nominal tick, so frames are identical at any speed.
export const TICK_MS = 300;
const PARALLEL = 2;

export const mockOptions = { tick: 300, freeze: null as number | null, holdStop: false, surfer: false, llmOffline: false, traceSigs: 0, traceCycles: 0 };
if (typeof location !== "undefined") {
  const q = new URLSearchParams(location.search);
  if (q.get("fast") === "1") mockOptions.tick = 0;
  if (q.has("tick")) mockOptions.tick = Math.max(0, Number(q.get("tick")) || 0);
  if (q.has("freeze")) mockOptions.freeze = Number(q.get("freeze"));
  if (q.get("holdStop") === "1") mockOptions.holdStop = true;
  if (q.get("surfer") === "1") mockOptions.surfer = true;
  if (q.get("llm") === "offline") mockOptions.llmOffline = true;
  if (q.get("design") === "fifo") mockSession.design.hierarchy = MOCK_FIFO;
  if (q.get("design") === "scan") mockSession.design.hierarchy = [];
  if (q.get("welcome") === "1") {
    mockSession.elaborated = false;
    mockSession.design = { files: [], top: null, defines: [], hierarchy: [] };
    mockSession.clocks = [];
    mockSession.reset = null;
    mockSession.props = [];
  }
  if (q.has("traceSigs") || q.has("traceCycles") || q.get("bigtrace") === "1") {
    mockOptions.traceSigs = Number(q.get("traceSigs") ?? (q.get("bigtrace") === "1" ? 500 : 0));
    mockOptions.traceCycles = Number(q.get("traceCycles") ?? (q.get("bigtrace") === "1" ? 5000 : 0));
  }
  // `?tasks=1`: a second task, FreeQ, with a copy of a_max and a cut point
  // on q (as `task -create FreeQ -copy a_max; stopat -task FreeQ q`).
  if (q.get("tasks") === "1") {
    mockSession.tasks = [{ name: "<embedded>", stopats: [] }, { name: "FreeQ", stopats: ["q"] }];
    mockSession.current_task = "<embedded>";
    mockSession.props.push({ name: "FreeQ::a_max", task: "FreeQ", kind: "assert", expr: "q <= 4'd9", enabled: true, status: { status: "unprocessed" } });
    mockResults["FreeQ::a_max"] = { status: "cex", engine: "ric3-bmc", depth: 1, ms: 18 };
    MOCK_ENGINES["FreeQ::a_max"] = [run("ric3-bmc", "won", 18, 1), run("ric3-ic3", "killed", 19, 1)];
    MOCK_STATS["FreeQ::a_max"] = { engine: "ric3-bmc", wall_ms: 18, cpu_ms: 40, depth: 1, depth_kind: "trace", max_bound: 1, evidence: "trace" };
  }
  const extra = Math.min(100_000, Math.max(0, Number(q.get("props")) || 0));
  if (extra) mockSession.props.push(...generatedProps(extra));
}

type Ev = "prove-progress" | "console-line" | "engine-log" | "ask-delta" | "ask-step" | "ask-done" | "ai-activity" | "ai-gui";
const listeners: Record<Ev, Set<(p: unknown) => void>> = {
  "prove-progress": new Set(),
  "console-line": new Set(),
  "engine-log": new Set(),
  "ask-delta": new Set(),
  "ask-step": new Set(),
  "ask-done": new Set(),
  "ai-activity": new Set(),
  "ai-gui": new Set(),
};
export const mockEvents = {
  on(ev: Ev, cb: (p: unknown) => void) {
    listeners[ev].add(cb);
    return () => void listeners[ev].delete(cb);
  },
  emit(ev: Ev, payload: ProveProgress | ConsoleLine | EngineLine | AskDelta | AskDone | GuiRequest | string) {
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
  // Runs before the reply: a sourced script's properties join the session.
  onEnd?: () => void;
}
let active: Run | null = null;

const isFinal = (p: PropProgress) => p.status !== "running" && p.status !== "unprocessed";
const summaryOf = (props: Property[]) =>
  WORDS.map((w) => `${w} ${props.filter((p) => p.status.status === w).length}`).join(" ");
// Replace a progress entry's fields, keeping its name and kind.
const reset = (p: PropProgress, to: Omit<PropProgress, "name" | "kind">) => {
  for (const k of Object.keys(p)) if (k !== "name" && k !== "kind") delete (p as unknown as Record<string, unknown>)[k];
  Object.assign(p, to);
};

// Raw engine output for the Log tab: one line when a property starts, one
// per tick, and the verdict line.
function engineLine(prop: string, engine: string, text: string, err = false) {
  mockEvents.emit("engine-log", { prop, engine, text, err });
}

// Tab completion: a static list of each flavor's commands, then the
// session's property names (crates/informal-fpv compat::complete in the app).
export const MOCK_COMMANDS: Record<string, string[]> = {
  jg: ["analyze", "assert", "assume", "clear", "clock", "cover", "elaborate", "get_property_list", "get_status", "include", "prove", "report", "reset", "set_prove_time_limit"],
  vcf: ["check_fv", "create_clock", "create_reset", "fvassert", "fvassume", "fvcover", "fvdisable", "fvenable", "get_props", "read_file", "report_fv", "set_fml_appmode", "set_fml_var"],
};
export function mockComplete(prefix: string): string[] {
  if (!prefix) return [];
  const cmds = (MOCK_COMMANDS[mockFlavor] ?? []).filter((c) => c.startsWith(prefix));
  const props = mockSession.props.map((p) => p.name).filter((n) => n.startsWith(prefix) && !cmds.includes(n)).sort();
  return [...cmds, ...props];
}

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
    const engine = mockResults[p.name]?.engine ?? "ric3";
    reset(p, { status: "running", engine, depth: 1, ms: 0 });
    run.ran.set(p.name, 0);
    engineLine(p.name, engine, `reading informal_work/${p.name}.btor2`);
  }
}

function finish(run: Run, kind: "done" | "stopped") {
  if (run.timer) clearTimeout(run.timer);
  active = null;
  emit(run, kind);
  run.onEnd?.();
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
      const prop = run.props.find((q) => q.name === p.name)!;
      prop.status = { ...final };
      prop.trace = MOCK_TRACES[p.name] ?? null;
      prop.engines = (MOCK_ENGINES[p.name] ?? []).map((e) => ({ ...e }));
      prop.stats = MOCK_STATS[p.name] ?? null;
      reset(p, { ...final, engines: prop.engines.map((e) => ({ ...e })) });
      engineLine(p.name, final.engine ?? "ric3", final.status === "proven" || final.status === "unreachable" ? "unsat" : "sat", false);
      mockEvents.emit("console-line", { kind: "out", text: lineFor(p.name, final) });
    } else {
      Object.assign(p, { depth: 1 + n * 5, ms: n * TICK_MS });
      engineLine(p.name, p.engine ?? "ric3", `frame ${1 + n * 5}`);
    }
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

function startProve(props: Property[], onEnd?: () => void): Promise<Reply> {
  return new Promise((resolve) => {
    const run: Run = {
      props,
      onEnd,
      prog: props.map((p) => ({ name: p.name, kind: p.kind === "cover" ? "cover" : "assert", status: "unprocessed" })),
      ran: new Map(),
      ticks: 0,
      timer: null,
      resolve,
    };
    for (const p of props) Object.assign(p, { status: { status: "unprocessed" }, trace: null, engines: [], stats: null });
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
  mockSession.props.length = BASE_PROPS;
  for (const p of mockSession.props) Object.assign(p, { enabled: true, status: { status: "unprocessed" }, trace: null, engines: [] });
  Object.assign(mockOptions, { tick: 300, freeze: null, holdStop: false, surfer: false, llmOffline: false });
  mockSurferOpened.length = 0;
  mockLlm.selected = "fake";
  for (const t of mockAsks.values()) clearTimeout(t);
  mockAsks.clear();
  mockAi.proposals.length = 0;
  mockUi = null;
  mockRtlOverride = null;
  guiWaiting.clear();
}

// ---- Ask mode: a scripted Fake provider ----

export const MOCK_ASK_REPLY = "Proving only that property:\n```tcl\nprove -property a_never9\n```";
const OLLAMA_DOWN = "http://localhost:11434/v1: Connection refused (browser preview)";
export const mockLlm = {
  selected: "fake",
  providers: [
    { name: "fake", kind: "fake", model: "scripted", base_url: "" },
    { name: "ollama-gemma", kind: "openai-compat", model: "gemma3:12b", base_url: "http://localhost:11434/v1" },
  ],
};
const mockAsks = new Map<number, ReturnType<typeof setTimeout>>();

export function mockLlmStatus(): LlmStatus {
  const p = mockLlm.providers.find((q) => q.name === mockLlm.selected)!;
  const down = mockOptions.llmOffline ? OLLAMA_DOWN : p.name === "ollama-gemma" ? OLLAMA_DOWN : undefined;
  return { provider: p.name, model: p.model, ok: !down, ...(down ? { error: down } : {}) };
}
export function mockLlmProviders(): LlmProviderEntry[] {
  return mockLlm.providers.map((p) => ({ ...p, selected: p.name === mockLlm.selected }));
}
export function mockLlmSelect(name: string): LlmStatus {
  if (!mockLlm.providers.some((p) => p.name === name)) throw new Error(`no LLM provider ${name}`);
  mockLlm.selected = name;
  return mockLlmStatus();
}

// The scripted answer: failures are listed from the session; any other
// question gets the a_never9 prove.
function mockAnswer(prompt: string): string {
  if (/fail|cex|wrong/i.test(prompt)) {
    const failed = mockSession.props.filter((p) => p.status.status === "cex").map((p) => p.name);
    if (!failed.length) return "No property has failed yet; none has a CEX in this session.";
    return `${failed.length === 1 ? "One property failed" : `${failed.length} properties failed`}: ${failed.join(", ")} (CEX).`;
  }
  return MOCK_ASK_REPLY;
}

export function mockAsk(id: number, prompt: string): number {
  const st = mockLlmStatus();
  if (!st.ok) throw new Error(st.error);
  // A "why" question is looked up first, as a model with tools would:
  // the failing property, then its trace.
  const why = /^\s*why\b/i.test(prompt);
  const failed = mockSession.props.find((p) => p.status.status === "cex");
  const lookups = why ? ["property_list status=cex", ...(failed ? [`trace_get name=${failed.name}`] : [])] : [];
  const text = why
    ? failed
      ? `${failed.name} fails: q reaches 9 in cycle ${failed.status.depth ?? 11}, which the assertion forbids.`
      : "No assertion has failed."
    : mockAnswer(prompt);
  const words = text.match(/\S+\s*/g) ?? [];
  const step = mockOptions.tick ? 30 : 0;
  let i = 0;
  const next = () => {
    const lookup = lookups.shift();
    if (lookup) {
      mockEvents.emit("ask-step", { id, text: lookup });
      mockAsks.set(id, setTimeout(next, step));
      return;
    }
    if (i < words.length) {
      mockEvents.emit("ask-delta", { id, text: words[i++] });
      mockAsks.set(id, setTimeout(next, step));
      return;
    }
    mockAsks.delete(id);
    const { explanation, command } = splitAnswer(text);
    mockEvents.emit("ask-done", { id, command, explanation, model: "scripted", first_token_ms: 0, total_ms: words.length * step });
  };
  mockAsks.set(id, setTimeout(next, step));
  return id;
}

export function mockAskCancel(id: number): boolean {
  const t = mockAsks.get(id);
  if (t === undefined) return false;
  clearTimeout(t);
  mockAsks.delete(id);
  mockEvents.emit("ask-done", { id, command: null, explanation: "", error: "cancelled", total_ms: 0 });
  return true;
}

// ---- AI interface: what an agent on the app's socket would have done ----
export const MOCK_PROPOSALS: Omit<AiProposal, "id" | "state">[] = [
  { kind: "assume", name: "m_en_hold", text: "en |=> en", rationale: "The counter is only specified while en stays high once raised; without it a_step has a counterexample." },
  { kind: "assert", name: "a_q_bound", text: "q <= 4'd9", rationale: "Helper invariant: q never leaves 0..9, which makes a_never9's failure the only reachable one." },
];
const mockAi: AiStatus = {
  mode: "propose",
  socket:
    typeof location !== "undefined" && new URLSearchParams(location.search).get("longSocket") === "1"
      ? "/var/folders/8x/9q2m4r7n6v0h3k1w5z8p0000gn/T/informal-socket-cache/informal-app-98765.sock"
      : "/run/user/1000/informal/app-1.sock",
  error: null,
  proposals: [],
};
// An agent proposes a change: it is pending, and the window is told.
export function mockAiPropose(p: Omit<AiProposal, "id" | "state">): AiProposal {
  const made: AiProposal = { ...p, id: mockAi.proposals.length + 1, state: "pending" };
  mockAi.proposals.push(made);
  mockEvents.emit("ai-activity", "proposal_add");
  return made;
}
if (typeof location !== "undefined" && new URLSearchParams(location.search).get("ai") === "1") {
  MOCK_PROPOSALS.forEach((p, i) => mockAi.proposals.push({ ...p, id: i + 1, state: "pending" }));
}
export function mockAiStatus(): AiStatus {
  return { ...mockAi, proposals: mockAi.proposals.map((p) => ({ ...p })) };
}
// The person's answer. Accepting adds the property, tagged AI-added; a
// name the session already has fails, as the session would refuse it.
export function mockAiDecide(id: number, accept: boolean): AiProposal {
  const p = mockAi.proposals.find((q) => q.id === id);
  if (!p) throw new Error(`no proposal ${id}`);
  if (p.state !== "pending") throw new Error(`proposal ${id} is ${p.state}`);
  if (!accept) p.state = "rejected";
  else if (p.kind === "cut_point" || p.kind === "command") p.state = "applied";
  else if (mockSession.props.some((q) => q.name === p.name)) {
    p.state = "failed";
    p.error = `property \`${p.name}\` already exists`;
  } else {
    mockSession.props.push({ name: p.name ?? `${p.kind}_ai_${id}`, kind: p.kind, expr: p.text, enabled: true, status: { status: "unprocessed" }, ai: true });
    p.state = "applied";
  }
  return { ...p };
}

// The window as an agent's tools see it: what the views last reported, and
// actions sent to them. `mockAiGui` resolves with the window's state once
// the views answer, as the agent's tool call would; it rejects with the
// views' reason. In the browser both are on `window.__informalMock`, for
// the Playwright suites to play the agent.
let mockUi: GuiState | null = null;
export const mockUiNow = () => mockUi;
export function mockUiState(state: GuiState) {
  mockUi = state;
}
const guiWaiting = new Map<number, { resolve: (s: GuiState) => void; reject: (e: Error) => void }>();
let guiNext = 1;
export function mockAiGui(action: GuiAction): Promise<GuiState> {
  const id = guiNext++;
  return new Promise((resolve, reject) => {
    guiWaiting.set(id, { resolve, reject });
    mockEvents.emit("ai-gui", { id, action });
  });
}
export function mockAiGuiDone(id: number, state: GuiState | null, error?: string) {
  const w = guiWaiting.get(id);
  guiWaiting.delete(id);
  if (state && !error) mockUi = state;
  if (error || !state) w?.reject(new Error(error ?? "the window gave no state"));
  else w?.resolve(state);
}
if (typeof window !== "undefined") {
  (window as unknown as Record<string, unknown>).__informalMock = { aiGui: mockAiGui, aiPropose: mockAiPropose, uiState: mockUiNow };
}

let mockFlavor = "jg";
const WORDS = ["proven", "cex", "covered", "unreachable", "undetermined", "error"] as const;

// Property names from `-property name`, `-property {a b}`, or a bare `{a b}`.
function names(script: string): string[] {
  const m = script.match(/-property\s+(?:\{([^}]*)\}|(\S+))/) ?? script.match(/^\s*\S+\s+(?:\{([^}]*)\}|(\S+))/);
  return m ? (m[1] ?? m[2]).split(/\s+/).filter(Boolean) : [];
}

let cachedCounterBin: Uint8Array | null = null;
export function mockTraceBin(path: string): Uint8Array | null {
  if (!Object.values(MOCK_TRACES).includes(path)) return null;
  if (mockOptions.traceSigs > 0 && mockOptions.traceCycles > 0) {
    return generateSyntheticIftr(mockOptions.traceSigs, mockOptions.traceCycles);
  }
  if (!cachedCounterBin) {
    const parsed = parseVcd(counterTrace);
    cachedCounterBin = encodeVcdToIftr(parsed, 12, "counter.clk");
  }
  return cachedCounterBin;
}

export function mockSchematic(name: string, maxDepth = 2): SchematicGraphView {
  const nodes: SchematicGraphView["nodes"] = [
    {
      id: "n_target",
      label: name,
      signal: name,
      kind: "target",
      op: "bad",
      width: 1,
      distance: 0,
      causal: true,
      hotspot: false,
      expandable: false,
      ports: [{ id: "n_target.chk", label: "chk", dir: "in" }],
    },
    {
      id: "n_cmp",
      label: "done (==)",
      signal: "counter.done",
      kind: "cmp",
      op: "eq",
      width: 1,
      distance: 0,
      causal: true,
      hotspot: false,
      expandable: false,
      ports: [
        { id: "n_cmp.a", label: "a", dir: "in" },
        { id: "n_cmp.b", label: "b", dir: "in" },
        { id: "n_cmp.y", label: "y", dir: "out" },
      ],
    },
    {
      id: "n_q",
      label: "q",
      signal: "counter.q",
      kind: "register",
      op: "state",
      width: 4,
      distance: 0,
      causal: true,
      hotspot: false,
      expandable: maxDepth === 0,
      ports: [
        ...(maxDepth > 0 ? [{ id: "n_q.d", label: "D", dir: "in" as const }] : []),
        { id: "n_q.y", label: "Q", dir: "out" },
      ],
    },
    {
      id: "n_c9",
      label: "4'd9",
      kind: "const",
      op: "const",
      width: 4,
      distance: 0,
      causal: true,
      hotspot: false,
      expandable: false,
      ports: [{ id: "n_c9.y", label: "y", dir: "out" }],
    },
  ];
  const edges: SchematicGraphView["edges"] = [
    { id: "e0", source: "n_cmp", source_port: "n_cmp.y", target: "n_target", target_port: "n_target.chk", width: 1, inverted: false, causal: true, signal: "counter.done" },
    { id: "e1", source: "n_q", source_port: "n_q.y", target: "n_cmp", target_port: "n_cmp.a", width: 4, inverted: false, causal: true, signal: "counter.q" },
    { id: "e2", source: "n_c9", source_port: "n_c9.y", target: "n_cmp", target_port: "n_cmp.b", width: 4, inverted: false, causal: true },
  ];
  if (maxDepth > 0) {
    nodes.push(
      {
        id: "n_mux",
        label: "MUX",
        kind: "mux",
        op: "ite",
        width: 4,
        distance: 1,
        causal: true,
        hotspot: false,
        expandable: false,
        ports: [
          { id: "n_mux.sel", label: "S", dir: "in" },
          { id: "n_mux.t", label: "1", dir: "in" },
          { id: "n_mux.f", label: "0", dir: "in" },
          { id: "n_mux.y", label: "Y", dir: "out" },
        ],
      },
      {
        id: "n_add",
        label: "+ 4'd1",
        kind: "arith",
        op: "add",
        width: 4,
        distance: 1,
        causal: true,
        hotspot: maxDepth >= 2,
        expandable: false,
        ports: [
          { id: "n_add.a", label: "a", dir: "in" },
          { id: "n_add.y", label: "y", dir: "out" },
        ],
      },
      {
        id: "n_en",
        label: "en",
        signal: "counter.en",
        kind: "input",
        op: "input",
        width: 1,
        distance: 1,
        causal: true,
        hotspot: false,
        expandable: false,
        ports: [{ id: "n_en.y", label: "y", dir: "out" }],
      },
    );
    edges.push(
      { id: "e3", source: "n_mux", source_port: "n_mux.y", target: "n_q", target_port: "n_q.d", width: 4, inverted: false, causal: true },
      { id: "e4", source: "n_en", source_port: "n_en.y", target: "n_mux", target_port: "n_mux.sel", width: 1, inverted: false, causal: true, signal: "counter.en" },
      { id: "e5", source: "n_add", source_port: "n_add.y", target: "n_mux", target_port: "n_mux.t", width: 4, inverted: false, causal: true },
      { id: "e6", source: "n_q", source_port: "n_q.y", target: "n_mux", target_port: "n_mux.f", width: 4, inverted: false, causal: false, signal: "counter.q" },
      { id: "e7", source: "n_q", source_port: "n_q.y", target: "n_add", target_port: "n_add.a", width: 4, inverted: false, causal: true, signal: "counter.q" },
    );
  }
  return { property: name, max_depth: maxDepth, truncated: false, nodes, edges };
}

export function mockFormalLint(file?: string): FormalLintFindingView[] {
  if (file && file !== MOCK_RTL) return [];
  const src = mockText(MOCK_RTL) ?? mockSource;
  if (src.includes("disable iff (!rst_n)")) return [];
  const idx = src.indexOf("@(posedge clk)");
  const insertAt = idx >= 0 ? idx + "@(posedge clk)".length : 0;
  return [
    {
      rule: "SV-FPV-003",
      name: "FormalAssertMissingDisableIff",
      severity: "info",
      message: "Concurrent property 'm_en' has no 'disable iff' in module 'counter' with reset 'rst_n'",
      file: MOCK_RTL,
      line: 7,
      column: 5,
      fixes: insertAt > 0
        ? [
            {
              file: MOCK_RTL,
              start: insertAt,
              end: insertAt,
              line: 7,
              replacement: " disable iff (!rst_n)",
              description: "Insert `disable iff (!rst_n)`",
              safety: "safe",
            },
          ]
        : [],
    },
  ];
}

export function mockApplyFormalFix(fix: FormalQuickFixView): { applied: boolean; updatedText?: string } {
  const cur = mockText(fix.file);
  if (cur === null) throw new Error(`${fix.file}: not in design`);
  const next = cur.slice(0, fix.start) + fix.replacement + cur.slice(fix.end);
  if (fix.file === MOCK_RTL) mockRtlOverride = next;
  return { applied: true, updatedText: next };
}

export function mockSignalNav(signal: string): SignalNavView {
  const leaf = signal.split(".").pop() ?? signal;
  if (leaf === "q") {
    return {
      signal,
      leaf: "q",
      declarations: [{ file: MOCK_RTL, line: 2, column: 34, role: "declaration", kind: "output", scope: "counter", snippet: "output logic [3:0] q, output logic done);" }],
      drivers: [
        { file: MOCK_RTL, line: 5, column: 21, role: "driver", kind: "always_ff", scope: "counter", snippet: "if (!rst_n) q <= 4'd0;" },
        { file: MOCK_RTL, line: 6, column: 30, role: "driver", kind: "always_ff", scope: "counter", snippet: "else if (en && !done) q <= q + 4'd1;" },
      ],
      loads: [{ file: MOCK_RTL, line: 3, column: 20, role: "load", kind: "expr", scope: "counter", snippet: "assign done = (q == 4'd9);" }],
    };
  }
  return {
    signal,
    leaf,
    declarations: [{ file: MOCK_RTL, line: 1, column: 16, role: "declaration", kind: "input", scope: "counter", snippet: `input logic ${leaf}` }],
    drivers: [],
    loads: [{ file: MOCK_RTL, line: 6, column: 18, role: "load", kind: "expr", scope: "counter", snippet: "else if (en && !done) q <= q + 4'd1;" }],
  };
}

export function mockLoadTemplate(templateId: string): Reply {
  mockSession.elaborated = true;
  mockSession.design = { files: [MOCK_RTL], top: "counter", defines: [], hierarchy: MOCK_HIERARCHY };
  mockSession.clocks = [{ signal: "clk", edge: "posedge" }];
  mockSession.reset = "!rst_n";
  mockSession.props = [
    { name: "a_max", source: at(MOCK_SCRIPT, 8), kind: "assert", expr: "q <= 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "a_step", source: at(MOCK_SCRIPT, 9), kind: "assert", expr: "en && !done |=> q == $past(q) + 4'd1", enabled: true, status: { status: "unprocessed" } },
    { name: "a_never9", source: at(MOCK_SCRIPT, 10), kind: "assert", expr: "q != 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "c_done", source: at(MOCK_SCRIPT, 11), kind: "cover", expr: "$rose(done)", enabled: true, status: { status: "unprocessed" } },
    { name: "c_10", source: at(MOCK_SCRIPT, 12), kind: "cover", expr: "q == 4'd10", enabled: true, status: { status: "unprocessed" } },
    { name: "m_en", source: at(MOCK_RTL, 7), kind: "assume", expr: "en |=> en", enabled: true, status: { status: "unprocessed" } },
  ];
  return { ok: true, result: `Loaded starter workspace: ${templateId}`, session: mockSession };
}

export function mockWhyCex(name: string): WhyCexReport {
  return {
    property: name,
    status: "cex",
    failure_frame: 10,
    total_frames: 11,
    static_cone_signals: 3,
    active_slice_signals: 2,
    active_signals: ["counter.en", "counter.q"],
    events: [
      { frame: 0, signal: "counter.q", kind: "register", width: 4, value: "4'h0", reason: "initial_state" },
      { frame: 1, signal: "counter.en", kind: "input", width: 1, value: "1'b1", reason: "active_input" },
      { frame: 2, signal: "counter.q", kind: "register", width: 4, value: "4'h1", prev_value: "4'h0", reason: "register_transition" },
      { frame: 10, signal: "counter.q", kind: "register", width: 4, value: "4'h9", prev_value: "4'h8", reason: "failure_operand" },
    ],
  };
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
  if (cmd === "shell_source" && String(args.path ?? "").endsWith(PROVE_ON_SOURCE)) {
    if (active) return { ok: false, result: "a prove is already running; stop it first", session: mockSession };
    const added = sourcedProps().filter((p) => !mockSession.props.some((q) => q.name === p.name));
    return startProve(added, () => mockSession.props.push(...added));
  }
  if (cmd === "shell_stop") {
    const run = active;
    if (!run) return { ok: true, result: "", session: mockSession };
    if (mockOptions.holdStop) return new Promise(() => {});
    stopProve(run);
    return { ok: true, result: "", session: mockSession };
  }
  // VC Formal only, as in crates/informal-fpv/src/compat.rs.
  const toggle = script.match(/^\s*(fvdisable|fvenable)\b/);
  if (cmd === "shell_eval" && toggle && mockFlavor === "vcf") {
    const want = names(script);
    const hit = mockSession.props.filter((p) => want.includes(p.name));
    if (!hit.length) return { ok: false, result: `no property matches \`${want.join(" ")}\``, session: mockSession };
    for (const p of hit) p.enabled = toggle[1] === "fvenable";
    return { ok: true, result: "", session: mockSession };
  }
  if (cmd === "shell_eval" && /^\s*check_cov\b/.test(script)) {
    mockSession.cov = {
      initialized: true,
      measured: /-measure\b/.test(script) || !!mockSession.cov?.measured,
      items: [
        { id: "coi_0", model: "coi", name: "counter.q", status: "covered", file: MOCK_RTL, line: 5 },
        { id: "br_0", model: "branch", name: "counter.br_reset", status: "covered", file: MOCK_RTL, line: 5 },
        { id: "br_1", model: "branch", name: "counter.br_inc", status: "covered", file: MOCK_RTL, line: 6 },
      ],
    };
    return { ok: true, result: "check_cov: 3/3 covered (0 UNR)", session: mockSession };
  }
  if (cmd === "shell_eval" && /^\s*check_xprop\b/.test(script)) {
    mockSession.xprop = {
      initialized: true,
      items: [
        { name: "counter.q", category: "output", status: "proven", engine: "informal-ic3-mt", ms: 4 },
        { name: "counter.done", category: "output", status: "proven", engine: "informal-ic3-mt", ms: 3 },
      ],
    };
    return { ok: true, result: "check_xprop: 2/2 proven (X-clean)", session: mockSession };
  }
  if (cmd === "shell_eval" && /^\s*check_spv\b/.test(script)) {
    if (/-clear\b/.test(script)) mockSession.spv = [];
    else if (/-prove\b/.test(script) && mockSession.spv) {
      mockSession.spv = mockSession.spv.map((r) => ({ ...r, status: "proven", engine: "informal-ic3-mt" }));
    } else if (/-create\b/.test(script)) {
      const fm = script.match(/-from\s+\{?([^\s}]+)\}?/)?.[1] ?? "en";
      const to = script.match(/-to\s+\{?([^\s}]+)\}?/)?.[1] ?? "done";
      mockSession.spv = [...(mockSession.spv ?? []), { name: `spv_${(mockSession.spv?.length ?? 0) + 1}`, task: "<embedded>", from: [fm], to: [to], status: "unprocessed" }];
    }
    return { ok: true, result: "", session: mockSession };
  }
  if (cmd === "shell_source" && String(args.path ?? "").startsWith("template:")) {
    return mockLoadTemplate(String(args.path).slice("template:".length));
  }
  if (cmd === "shell_source" && !mockSession.elaborated) {
    return mockLoadTemplate(String(args.path ?? "counter.jg.tcl"));
  }
  if (cmd === "shell_eval" && /^\s*why_cex\b/.test(script)) {
    return { ok: true, result: JSON.stringify(mockWhyCex("a_never9")), session: mockSession };
  }
  if (cmd === "shell_eval" && script.trim() !== "") {
    return { ok: false, result: "browser preview: Tcl runs only in the desktop app", session: mockSession };
  }
  return { ok: true, result: "", session: mockSession };
}
