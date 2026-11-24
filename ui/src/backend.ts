// Calls into the Rust core. Inside the desktop app this goes through Tauri
// commands (src-tauri/src/main.rs); in a plain browser (`npm run dev`) a
// small mock stands in so the views can be worked on without the app.

import { mock, mockApplyFormalFix, mockDesignInfo, mockAiDecide, mockAiGuiDone, mockAiStatus, mockAsk, mockAskCancel, mockComplete, mockEvents, mockFormalLint, mockLlmProviders, mockLlmSelect, mockLlmStatus, mockOpenInSurfer, mockOptions, mockSchematic, mockSignalNav, mockText, mockTrace, mockTraceBin, mockUiState, mockWhyCex } from "./mock";
import type { AskDelta, AskDone, LlmProviderEntry, LlmStatus } from "./ask";
import { isMcpAppSurface, mcpBridge } from "./mcpApp";
import type { TraceAnnotation } from "./traceBin";
import type { FocusChip, GuiCaptureSnapshot, LayoutPreset, WalkthroughStep } from "./store/workbenchStore";

export type StatusWord =
  | "unprocessed" | "proven" | "bounded_proven" | "cex" | "covered" | "unreachable" | "undetermined" | "error";

export type EffortLevel = "low" | "standard" | "high" | "max";
export type OrchestrationMode = "auto" | "on" | "off";

export interface PropStatus {
  status: StatusWord;
  engine?: string;
  depth?: number | null;
  bound?: number | null;
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
  // workdir, e.g. "informal_work/a_never9.vcd"); absent or null when there is
  // none. See docs/ui/trace-format.md.
  trace?: string | null;
  // Where the property was written: the script line of the Tcl command that
  // created it (absolute path, 1-based line), or an RTL file:line for SVA
  // in the design. Absent or null for properties typed at the console; the
  // UI then falls back to searching the top file (model.ts locate).
  source?: SourceLoc | null;
  // The last prove's portfolio record, one entry per engine that ran (the
  // proof profiler). Absent or empty before a prove.
  engines?: EngineRun[];
  // Added through the AI interface (decision D17), not by the user.
  ai?: boolean;
  // The AI-added assumptions (by name) and cut points ("stopat <signal>")
  // that were in the model of the last prove: the result stands on them.
  under_ai?: string[];
  // The task it belongs to (decision D20); absent from older backends,
  // which have only "<embedded>". Outside it the name is "<task>::<name>".
  task?: string;
  // A copy of a property written in the RTL: the original's name.
  copy_of?: string | null;
  // What the last prove measured (informal-fpv stats.rs); absent before
  // a prove and when no engine ran.
  stats?: ProofStats | null;
  // What the vacuity checks found for a proven assertion
  // ("non-vacuous", "vacuous", ...); absent when they did not look.
  vacuity?: string | null;
  witness_trace?: string | null;
  bound?: number | null;
  proof_core?: {
    states: string[];
    inputs: string[];
    needed_assumptions: string[];
    unused_assumptions: string[];
  } | null;
}

// Per property, what the last prove measured. Only measured values are
// present: a field the engines did not report is absent.
export interface ProofStats {
  engine?: string;
  wall_ms: number;
  // Process CPU time, engine processes included; can exceed wall time.
  cpu_ms?: number;
  // Peak resident memory, only when the high-water mark rose during this
  // property's run.
  peak_rss_kb?: number;
  depth?: number;
  // What `depth` counts: trace length, k of k-induction, IC3 frames, or
  // the BMC bound an undetermined run reached.
  depth_kind?: "trace" | "k" | "frames" | "bound";
  max_bound?: number;
  invariant_clauses?: number;
  evidence?: string;
  certificate?: string;
}

// `get_design_info -json` (informal-fpv stats.rs DesignInfo).
export interface DesignCounts {
  instances: number;
  modules: number;
  inputs: number;
  input_bits: number;
  outputs: number;
  output_bits: number;
  flops: number;
  flop_bits: number;
  latches: number;
  latch_bits: number;
  memories: { name: string; words: number; width: number }[];
  clocks: { signal: string; edges: string[]; registers: number }[];
  resets: { signal: string; active_high: boolean; registers: number }[];
  undriven: number;
}
export interface ModelCounts {
  nodes: number;
  states: number;
  state_bits: number;
  array_states: number;
  inputs: number;
  input_bits: number;
  constraints: number;
  bad: number;
  justice: number;
  aig?: { inputs: number; latches: number; ands: number };
  aig_error?: string;
}
export interface DesignInfo {
  top: string | null;
  files: number;
  elaborated: boolean;
  design: DesignCounts | null;
  declared_clocks: string[];
  declared_reset: string | null;
  black_boxes: string[];
  task: string;
  cut_points: string[];
  properties: { asserts: number; covers: number; assumptions: number; disabled: number };
  model: ModelCounts | null;
  model_error?: string;
}

// A change an agent asked for through the AI interface
// (docs/ai-interface.md). "pending" waits for the person's answer.
export type ProposalState = "pending" | "applied" | "rejected" | "withdrawn" | "undone" | "failed" | "gone";
export interface AiProposal {
  id: number;
  kind: "assume" | "assert" | "cover" | "cut_point" | "command";
  name?: string;
  // The property expression, the signal to cut, or the Tcl command.
  text: string;
  rationale: string;
  state: ProposalState;
  error?: string;
}
// What the window shows, as an agent's window tools read it (`gui_state`).
export interface GuiState {
  selected_property: string | null;
  view: "table" | "source" | "trace" | "schematic" | "apps";
  filter: string;
  status_filter?: string | null;
  layout_preset?: LayoutPreset | null;
  active_app?: string | null;
  bottom_tab?: string | null;
  follow_agent?: boolean | null;
  // The open trace: its property, the cycle the cursor is on, the signals
  // in the waveform (hierarchical paths), causal slice state, and AI callouts.
  trace: {
    property: string;
    cursor: number;
    signals: string[];
    why_cex_open?: boolean;
    slice_only?: boolean;
    annotations?: TraceAnnotation[];
  } | null;
  // The source in view and its highlighted lines.
  source: { file: string; first_line: number; last_line: number } | null;
  schematic?: { property: string | null; max_depth: number } | null;
  walkthrough?: { title: string; step: number; total_steps: number; label: string } | null;
  focus_context?: FocusChip[] | null;
  capture?: GuiCaptureSnapshot | null;
}
// One thing an agent asks the window to do (docs/ai-interface.md).
export type GuiAction =
  | { action: "select_property"; name: string }
  | { action: "open_trace"; name: string; frame?: number | null }
  | { action: "add_signals"; signals: string[] }
  | { action: "highlight_source"; file: string; first_line: number; last_line: number }
  | {
      action: "set_view";
      view?: "table" | "source" | "trace" | "schematic" | "apps" | null;
      layout_preset?: LayoutPreset | null;
      app_tab?: string | null;
      bottom_tab?: string | null;
      filter?: string | null;
      status_filter?: string | null;
    }
  | { action: "toggle_why_cex"; open: boolean; slice_only?: boolean | null }
  | { action: "annotate_trace"; annotations?: TraceAnnotation[]; clear?: boolean }
  | {
      action: "present_walkthrough";
      title?: string | null;
      steps?: WalkthroughStep[] | null;
      step?: number | null;
      clear?: boolean;
    }
  | { action: "open_schematic"; name?: string | null; max_depth?: number | null }
  | {
      action: "present_cex";
      name: string;
      frame?: number | null;
      layout_preset?: LayoutPreset | null;
      slice_only?: boolean | null;
      title?: string | null;
      steps?: WalkthroughStep[] | null;
      annotations?: TraceAnnotation[] | null;
      capture?: boolean | null;
    }
  | { action: "capture_view"; target?: "trace" | "schematic" | "window" };
export interface GuiRequest {
  id: number;
  action: GuiAction;
}
export interface AiStatus {
  mode: "read-only" | "propose" | "auto";
  // Where agents connect (`informal mcp --connect`); null when it is off.
  socket: string | null;
  error: string | null;
  proposals: AiProposal[];
}

// How one portfolio engine's run on a property ended (informal-engine
// portfolio::EngineRun): the winner, a loser that finished, one out of
// time, one cancelled by the winner or a Stop, or one that failed to run.
export type RunOutcome = "won" | "lost" | "timeout" | "killed" | "error";
export interface EngineRun {
  engine: string;
  outcome: RunOutcome;
  // Wall time from the start of the portfolio until the engine returned.
  ms: number;
  // CEX depth for an unsafe verdict, else the deepest bound it reported.
  depth?: number | null;
}

export interface SourceLoc {
  file: string;
  line: number;
}

// One instance in the design's scope tree (informal_svlang::Scope): its ports and
// named signals, and its child instances. Signal names are relative to the
// scope; `path + "." + name` is the hierarchical name (and the VCD path).
export interface ScopeSignal {
  name: string;
  width: number;
  kind: "port" | "variable" | "net" | "parameter";
  // Ports only.
  dir?: "input" | "output" | "inout" | "ref";
}
export interface DesignScope {
  path: string;
  name: string;
  module: string;
  signals: ScopeSignal[];
  children: DesignScope[];
}

export interface CausalEvent {
  frame: number;
  signal: string;
  kind: string;
  width: number;
  value: string;
  prev_value?: string;
  reason: string;
}

export interface WhyCexReport {
  property: string;
  status: string;
  failure_frame: number;
  total_frames: number;
  static_cone_signals: number;
  active_slice_signals: number;
  active_signals: string[];
  events: CausalEvent[];
}

export interface CovItemView {
  id: string;
  model: string;
  name: string;
  status: string;
  file?: string;
  line?: number;
}

export interface CovStateView {
  initialized: boolean;
  measured: boolean;
  items: CovItemView[];
}

export interface XpropItemView {
  name: string;
  category: string;
  status: string | { cex?: { depth?: number | null } };
  engine?: string | null;
  ms?: number;
  trace?: string | null;
}

export interface XpropStateView {
  initialized: boolean;
  items: XpropItemView[];
}

export interface SpvRuleView {
  name: string;
  task: string;
  from: string[];
  to: string[];
  cond?: string | null;
  status: string | { cex?: { depth?: number | null } };
  engine?: string | null;
  trace?: string | null;
}

export interface SecCheckView {
  name: string;
  kind: string;
  spec_sig: string;
  imp_sig: string;
  width: number;
  latency: number;
  status: string | { cex?: { depth?: number | null } };
  engine?: string | null;
  trace?: string | null;
}

export interface SecStateView {
  setup?: { spec_top?: string | null; imp_top?: string | null; lec: boolean; latency: number } | null;
  checks?: SecCheckView[];
}

export interface SchematicPortView {
  id: string;
  label: string;
  dir: "in" | "out";
}
export interface SchematicNodeView {
  id: string;
  label: string;
  signal?: string;
  kind: "input" | "register" | "mux" | "arith" | "cmp" | "logic" | "const" | "target";
  op: string;
  width: number;
  distance: number;
  causal: boolean;
  hotspot: boolean;
  expandable: boolean;
  ports: SchematicPortView[];
}
export interface SchematicEdgeView {
  id: string;
  source: string;
  source_port: string;
  target: string;
  target_port: string;
  width: number;
  inverted: boolean;
  causal: boolean;
  signal?: string;
}
export interface SchematicGraphView {
  property: string;
  max_depth: number;
  truncated: boolean;
  nodes: SchematicNodeView[];
  edges: SchematicEdgeView[];
}

export interface FormalQuickFixView {
  file: string;
  start: number;
  end: number;
  line: number;
  replacement: string;
  description: string;
  safety: "safe" | "unsafe" | string;
}
export interface FormalLintFindingView {
  rule: string;
  name: string;
  severity: "error" | "warning" | "info" | "style" | string;
  message: string;
  file: string;
  line: number;
  column: number;
  fixes: FormalQuickFixView[];
}
export interface SignalRefView {
  file: string;
  line: number;
  column: number;
  role: "declaration" | "driver" | "load" | string;
  kind: string;
  scope: string;
  snippet: string;
}
export interface SignalNavView {
  signal: string;
  leaf: string;
  declarations: SignalRefView[];
  drivers: SignalRefView[];
  loads: SignalRefView[];
}

export interface EnvironmentView {
  task: string;
  asked: number;
  allowed: number | null;
  assumptions: string[];
  conflict_set: string[];
}

export interface SessionView {
  // `hierarchy`: the scope tree after elaboration; empty or absent with
  // a session elaborated without a SystemVerilog front end (the Design tab then scans the top file's ports).
  design: { files: string[]; top: string | null; defines: [string, string | null][]; hierarchy?: DesignScope[] };
  elaborated: boolean;
  clocks: { signal: string; edge: string }[];
  reset: string | null;
  props: Property[];
  // The tasks, "<embedded>" first, and the current one (absent from older
  // backends: one task).
  tasks?: { name: string; stopats: string[] }[];
  current_task?: string;
  environment?: EnvironmentView | null;
  time_limit: { secs: number; nanos: number };
  total_time_limit?: { secs: number; nanos: number } | null;
  effort?: EffortLevel;
  orchestration?: OrchestrationMode;
  max_trace_length?: number | null;
  workdir: string;
  cov?: CovStateView;
  xprop?: XpropStateView;
  spv?: SpvRuleView[];
  sec?: SecStateView;
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
  // Present once the property finished (the portfolio's per-engine record).
  engines?: EngineRun[];
}
export type ProvePhase = "sweep" | "converge" | "vacuity" | "witness" | "proof_core";
export interface ProveProgress {
  kind: ProgressKind;
  total: number;
  done: number;
  elapsed_ms: number;
  phase?: ProvePhase;
  props: PropProgress[];
}
// One line of engine output for the console (Tauri event "console-line").
export interface ConsoleLine {
  text: string;
  kind: "out" | "err";
}

// One line of an engine's own output (Tauri event "engine-log"): external
// engines' stdout/stderr, streamed while they run (up to 2,000 lines per
// stream per engine run). Engines that block-buffer a pipe (C programs
// such as ABC) deliver theirs when they exit.
export interface EngineLine {
  prop: string;
  engine: string;
  text: string;
  err: boolean;
}

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const httpBridge =
  typeof window !== "undefined"
    ? new URLSearchParams(window.location.search).get("bridge")
    : null;

async function bridgeRpc<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  const res = await fetch(`${httpBridge}/rpc`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cmd, args }),
  });
  if (!res.ok) {
    throw new Error(`bridge ${cmd}: HTTP ${res.status} ${await res.text()}`);
  }
  return (await res.json()) as T;
}

async function call(cmd: string, args: Record<string, unknown> = {}): Promise<Reply> {
  if (inTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<Reply>(cmd, args);
  }
  if (httpBridge) {
    return bridgeRpc<Reply>(cmd, args);
  }
  return mock(cmd, args);
}

// Subscribe to a backend event; returns an unsubscribe function that is safe
// to call before the Tauri listener has finished registering.
function subscribe<T>(event: "prove-progress" | "console-line" | "engine-log" | "ask-delta" | "ask-step" | "ask-done" | "ai-activity" | "ai-gui", cb: (payload: T) => void): () => void {
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

// A Tauri command, or the mock's answer in the browser.
async function invokeOr<T>(cmd: string, args: Record<string, unknown>, mocked: () => T): Promise<T> {
  if (inTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  }
  if (httpBridge) {
    return bridgeRpc<T>(cmd, args);
  }
  await new Promise((r) => setTimeout(r, 0));
  return mocked();
}

export const backend = {
  inTauri,
  inRealBridge: Boolean(httpBridge),
  inMcpApp: isMcpAppSurface,
  snapshot: async (): Promise<Reply> => {
    if (isMcpAppSurface) {
      try {
        const res = await mcpBridge.callTool<{ session: SessionView }>("open_workbench", {});
        return { ok: true, result: "", session: res.session };
      } catch {
        return call("snapshot");
      }
    }
    return call("snapshot");
  },
  // The flavor the shell started in (a startup script picks it).
  flavor: () => invokeOr<Flavor>("current_flavor", {}, () => "jg"),
  evalTcl: (script: string) => call("shell_eval", { script }),
  // The Design Info panel: the design's counts and the current task's
  // model size. Builds the model, so it is asked for, not polled.
  designInfo: async (): Promise<DesignInfo> => {
    if (!inTauri && !httpBridge) return mockDesignInfo();
    const r = await call("shell_eval", { script: "get_design_info -json" });
    if (!r.ok) throw new Error(r.result);
    return JSON.parse(r.result) as DesignInfo;
  },
  source: (path: string) => call("shell_source", { path }),
  setFlavor: (flavor: Flavor) => call("shell_flavor", { flavor }),
  // Ask a running prove to stop. The prove's own call then resolves with the
  // session; a "stopped" progress event marks the moment it took effect.
  stop: () => call("shell_stop"),
  onProgress: (cb: (p: ProveProgress) => void) => subscribe("prove-progress", cb),
  onConsole: (cb: (l: ConsoleLine) => void) => subscribe("console-line", cb),
  onEngineLog: (cb: (l: EngineLine) => void) => subscribe("engine-log", cb),
  // ---- Ask mode (docs/ui/ask-mode.md) ----
  // The LLM provider in use and whether it answers.
  llmStatus: () => invokeOr<LlmStatus>("llm_status", {}, () => mockLlmStatus()),
  llmProviders: () => invokeOr<LlmProviderEntry[]>("llm_providers", {}, () => mockLlmProviders()),
  // Switch provider for this run of the app (config/llm.toml is not written).
  llmSelect: (name: string) => invokeOr<LlmStatus>("llm_select", { name }, () => mockLlmSelect(name)),
  // Start an answer; it streams as onAskDelta events and ends with one
  // onAskDone, both tagged with `id` (chosen here, so no event is missed).
  // The backend never runs the suggested command.
  ask: (id: number, prompt: string, context?: string) =>
    invokeOr<number>("ask", { id, prompt, context: context ?? null }, () => mockAsk(id, prompt)),
  askCancel: (id: number) => invokeOr<boolean>("ask_cancel", { id }, () => mockAskCancel(id)),
  onAskDelta: (cb: (d: AskDelta) => void) => subscribe("ask-delta", cb),
  // A tool the model used before answering (it only reads the session):
  // `text` is the call on one line, such as "trace_get name=a_never9".
  onAskStep: (cb: (d: AskDelta) => void) => subscribe("ask-step", cb),
  onAskDone: (cb: (d: AskDone) => void) => subscribe("ask-done", cb),
  // ---- AI interface (docs/ai-interface.md) ----
  // The mode, the socket and every proposal with its state.
  aiStatus: () => invokeOr<AiStatus>("ai_status", {}, () => mockAiStatus()),
  // The person's answer to a pending proposal; resolves with the proposal
  // as it then stands (accepting can fail: state "failed", with `error`).
  aiDecide: (id: number, accept: boolean) => invokeOr<AiProposal>("ai_decide", { id, accept }, () => mockAiDecide(id, accept)),
  // An agent's call may have changed the session or the proposals: the
  // payload is the tool's name. Redraw from a snapshot and aiStatus.
  onAiActivity: (cb: (tool: string) => void) => subscribe("ai-activity", cb),
  // An agent's window tool asks the views to do something; answer each
  // request with aiGuiDone: the window's state after it, or why not.
  onAiGui: (cb: (r: GuiRequest) => void) => subscribe("ai-gui", cb),
  aiGuiDone: (id: number, state: GuiState | null, error?: string) =>
    invokeOr<void>("ai_gui_done", { id, state, error: error ?? null }, () => mockAiGuiDone(id, state, error)),
  // What the window shows, reported after every change for `gui_state`.
  uiState: (state: GuiState) => invokeOr<void>("ui_state", { state }, () => mockUiState(state)),
  // Tab completion: the active flavor's command names, then property names,
  // starting with `prefix`. Answered without the shell thread, so it works
  // while a prove runs.
  complete: async (prefix: string): Promise<string[]> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<string[]>("shell_complete", { prefix });
    }
    if (httpBridge) {
      return bridgeRpc<string[]>("shell_complete", { prefix });
    }
    return mockComplete(prefix);
  },
  // A design file or script for the source view. The app only reads files
  // under its working directory (the repo) or in the design's file list.
  readText: async (path: string): Promise<string> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<string>("read_text", { path });
    }
    if (httpBridge) {
      return bridgeRpc<string>("read_text", { path });
    }
    const t = mockText(path);
    if (t === null) throw new Error(`${path}: no such file`);
    return t;
  },
  // Whether an external tool (e.g. "surfer") is on PATH. Asked once at
  // start-up; the browser mock says no (yes with ?surfer=1).
  toolAvailable: async (name: string): Promise<boolean> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<boolean>("tool_available", { name });
    }
    return name === "surfer" && mockOptions.surfer;
  },
  // Open a trace in Surfer, detached. The app only opens .vcd files inside
  // the session's workdir; it rejects with the reason otherwise.
  openInSurfer: async (path: string): Promise<void> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<void>("open_in_surfer", { path });
    }
    return mockOpenInSurfer(path);
  },
  // Dynamic backward causal root-cause slice of a CEX or covered witness trace.
  whyCex: async (name: string): Promise<WhyCexReport> => {
    if (isMcpAppSurface) {
      try {
        return await mcpBridge.callTool<WhyCexReport>("why_cex", { name });
      } catch {
        // fallback in mock
      }
    }
    if (!inTauri && !httpBridge) return mockWhyCex(name);
    const r = await call("shell_eval", { script: `why_cex -json {${name}}` });
    if (!r.ok) throw new Error(r.result);
    return JSON.parse(r.result) as WhyCexReport;
  },
  // The VCD text of a property's trace. The app only reads files inside the
  // session's workdir; the mock serves tests/fixtures/a_never9.vcd.
  readTrace: async (path: string): Promise<string> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<string>("read_trace", { path });
    }
    if (httpBridge) {
      return bridgeRpc<string>("read_trace", { path });
    }
    const t = mockTrace(path);
    if (t === null) throw new Error(`${path}: no such trace`);
    return t;
  },
  // Binary `IFTR` v1 waveform stream over raw Tauri v2 IPC (`Uint8Array`).
  readTraceBin: async (path: string): Promise<Uint8Array> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      const ab = await invoke<ArrayBuffer | Uint8Array>("read_trace_bin", { path });
      return ab instanceof Uint8Array ? ab : new Uint8Array(ab);
    }
    const bin = mockTraceBin(path);
    if (bin === null) throw new Error(`${path}: no such trace`);
    return bin;
  },
  // Port-aware RTL COI & causal schematic subgraph (`elkjs` + Canvas).
  schematic: async (name: string, maxDepth = 2, maxNodes = 80): Promise<SchematicGraphView> => {
    if (isMcpAppSurface) {
      try {
        return await mcpBridge.callTool<SchematicGraphView>("schematic_get", {
          name,
          max_depth: maxDepth,
          max_nodes: maxNodes,
        });
      } catch {
        // fallback in mock
      }
    }
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<SchematicGraphView>("workbench_tool", {
        tool: "schematic_get",
        args: { name, max_depth: maxDepth, max_nodes: maxNodes },
      });
    }
    if (httpBridge) {
      return bridgeRpc<SchematicGraphView>("workbench_tool", {
        tool: "schematic_get",
        args: { name, max_depth: maxDepth, max_nodes: maxNodes },
      });
    }
    return mockSchematic(name, maxDepth);
  },
  // Live `svlang lint` (`formal-ready` `SV-FPV-001..005` + quick-fixes).
  formalLint: async (file?: string): Promise<FormalLintFindingView[]> => {
    if (isMcpAppSurface) {
      try {
        const res = await mcpBridge.callTool<{ findings: FormalLintFindingView[] }>(
          "formal_lint",
          file ? { file } : {},
        );
        return res.findings ?? [];
      } catch {
        // fallback in mock
      }
    }
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      const res = await invoke<{ findings: FormalLintFindingView[] }>("workbench_tool", {
        tool: "formal_lint",
        args: file ? { file } : {},
      });
      return res.findings ?? [];
    }
    if (httpBridge) {
      const res = await bridgeRpc<{ findings: FormalLintFindingView[] }>("workbench_tool", {
        tool: "formal_lint",
        args: file ? { file } : {},
      });
      return res.findings ?? [];
    }
    return mockFormalLint(file);
  },
  applyFormalFix: async (
    fix: FormalQuickFixView,
    rationale?: string,
  ): Promise<{ applied: boolean; updatedText?: string }> => {
    if (isMcpAppSurface) {
      const res = await mcpBridge.callTool<{ applied?: boolean }>("formal_fix_apply", {
        file: fix.file,
        start: fix.start,
        end: fix.end,
        replacement: fix.replacement,
        rationale: rationale ?? fix.description,
      });
      return { applied: Boolean(res.applied) };
    }
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      const res = await invoke<{ applied?: boolean }>("workbench_tool", {
        tool: "formal_fix_apply",
        args: {
          file: fix.file,
          start: fix.start,
          end: fix.end,
          replacement: fix.replacement,
          rationale: rationale ?? fix.description,
        },
      });
      return { applied: Boolean(res.applied) };
    }
    if (httpBridge) {
      const res = await bridgeRpc<{ applied?: boolean }>("workbench_tool", {
        tool: "formal_fix_apply",
        args: {
          file: fix.file,
          start: fix.start,
          end: fix.end,
          replacement: fix.replacement,
          rationale: rationale ?? fix.description,
        },
      });
      return { applied: Boolean(res.applied) };
    }
    return mockApplyFormalFix(fix);
  },
  // RTL Go-to-Driver and Go-to-Load navigation.
  signalNav: async (signal: string): Promise<SignalNavView> => {
    if (isMcpAppSurface) {
      try {
        return await mcpBridge.callTool<SignalNavView>("signal_nav", { signal });
      } catch {
        // fallback in mock
      }
    }
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<SignalNavView>("workbench_tool", {
        tool: "signal_nav",
        args: { signal },
      });
    }
    if (httpBridge) {
      return bridgeRpc<SignalNavView>("workbench_tool", {
        tool: "signal_nav",
        args: { signal },
      });
    }
    return mockSignalNav(signal);
  },
  // Pop out Trace or Schematic onto a secondary monitor window.
  popOutWindow: async (kind: "trace" | "schematic", title: string): Promise<void> => {
    if (inTauri) {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke<void>("pop_out_window", { kind, title });
      return;
    }
    if (typeof window !== "undefined") {
      window.open(`/?popout=${encodeURIComponent(kind)}&fast=1`, `informal-popout-${kind}`, "width=1120,height=680");
    }
  },
  // MCP App host hooks (SEP-1865 + Antigravity extensions).
  onMcpToolResult: (cb: (sc: Record<string, unknown>) => void) => mcpBridge.onToolResult(cb),
  attachHostContext: (summaryText: string) => mcpBridge.attachContext(summaryText),
  openHostEditorFile: (absPath: string) => mcpBridge.openFileInHostEditor(absPath),
};
