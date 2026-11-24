// Layer 3 — Full <App /> User-Event & Async Race/Escape Priority Suite
// Mounts <App /> with controllable deferred RPC promises to deterministically test:
//   1. Rapid clicks while busy=true (in-flight lockouts prevent duplicate/dropped RPCs)
//   2. Full Escape dismissal priority chain while a prove is running:
//        ShortcutSheet -> BudgetPopover -> ContextMenu -> Filter input -> Stop prove
//   3. Keyboard-only triage -> Source pivot -> ContextBar -> Alt+Left back restoration

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ProveProgress, Reply, SessionView } from "./backend";
import { useWorkbenchStore } from "./store/workbenchStore";

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const makeBaseSession = (): SessionView => ({
  design: {
    files: ["counter.sv"],
    top: "counter",
    defines: [],
    hierarchy: [
      {
        path: "counter",
        name: "counter",
        module: "counter",
        children: [],
        signals: [
          { name: "clk", width: 1, kind: "port", dir: "input" },
          { name: "q", width: 4, kind: "port", dir: "output" },
        ],
      },
    ],
  },
  elaborated: true,
  clocks: [{ signal: "clk", edge: "posedge" }],
  reset: "!rst_n",
  props: [
    {
      name: "a_max",
      kind: "assert",
      expr: "q <= 4'd9",
      enabled: true,
      status: { status: "proven", engine: "ric3-ic3", ms: 20 },
      vacuity: "non-vacuous",
      source: { file: "counter.sv", line: 4 },
    },
    {
      name: "a_vac",
      kind: "assert",
      expr: "1'b0 |-> q == 4'd0",
      enabled: true,
      status: { status: "proven", engine: "informal-kind", ms: 5 },
      vacuity: "vacuous",
      source: { file: "counter.sv", line: 5 },
    },
    {
      name: "a_never9",
      kind: "assert",
      expr: "q != 4'd9",
      enabled: true,
      status: { status: "cex", engine: "ric3-bmc", depth: 11, ms: 15 },
      trace: "informal_work/a_never9.vcd",
      source: { file: "counter.sv", line: 6 },
    },
    {
      name: "m_en",
      kind: "assume",
      expr: "en |=> en",
      enabled: true,
      status: { status: "unprocessed" },
      source: { file: "counter.sv", line: 7 },
    },
  ],
  tasks: [{ name: "<embedded>", stopats: [] }],
  current_task: "<embedded>",
  time_limit: { secs: 60, nanos: 0 },
  total_time_limit: null,
  effort: "standard",
  orchestration: "auto",
  max_trace_length: null,
  workdir: "informal_work",
});

const progressListeners = new Set<(p: ProveProgress) => void>();
let currentSession: SessionView = makeBaseSession();
let evalTclImpl: (cmd: string) => Promise<Reply> = async () => ({ ok: true, result: "", session: currentSession });
const evalTclSpy = vi.fn((cmd: string) => evalTclImpl(cmd));
const stopSpy = vi.fn(async (): Promise<Reply> => ({ ok: true, result: "", session: currentSession }));

vi.mock("./backend", () => ({
  backend: {
    inTauri: false,
    inMcpApp: false,
    snapshot: vi.fn(async () => ({ ok: true, result: "", session: currentSession })),
    flavor: vi.fn(async () => "jg" as const),
    setFlavor: vi.fn(async () => ({ ok: true, result: "", session: currentSession })),
    evalTcl: (cmd: string) => evalTclSpy(cmd),
    source: vi.fn(async () => ({ ok: true, result: "", session: currentSession })),
    stop: () => stopSpy(),
    complete: vi.fn(async () => []),
    readText: vi.fn(async () => "module counter(input logic clk, output logic [3:0] q);\nendmodule\n"),
    readTrace: vi.fn(async () => "$timescale 1ns $end\n$var wire 1 ! clk $end\n$enddefinitions $end\n#0\n0!\n#5\n1!\n"),
    readTraceBin: vi.fn(async () => null),
    toolAvailable: vi.fn(async () => false),
    openInSurfer: vi.fn(async () => {}),
    designInfo: vi.fn(async () => ({ top: "counter", files: 1, elaborated: true, design: null, declared_clocks: ["clk"], declared_reset: "!rst_n", black_boxes: [], task: "<embedded>", cut_points: [], properties: { asserts: 3, covers: 0, assumptions: 1, disabled: 0 }, model: null })),
    whyCex: vi.fn(async () => ({ property: "a_never9", status: "cex", failure_frame: 10, total_frames: 11, static_cone_signals: 2, active_slice_signals: 1, active_signals: ["counter.q"], events: [] })),
    schematic: vi.fn(async (name: string) => ({ property: name, max_depth: 2, truncated: false, nodes: [], edges: [] })),
    formalLint: vi.fn(async () => []),
    applyFormalFix: vi.fn(async () => ({ applied: true })),
    signalNav: vi.fn(async () => ({ signal: "q", leaf: "q", declarations: [], drivers: [], loads: [] })),
    llmStatus: vi.fn(async () => ({ provider: "fake", model: "scripted", ok: true })),
    llmProviders: vi.fn(async () => []),
    llmSelect: vi.fn(async () => ({ provider: "fake", model: "scripted", ok: true })),
    ask: vi.fn(async () => 1),
    askCancel: vi.fn(async () => true),
    aiStatus: vi.fn(async () => ({ mode: "propose", socket: null, error: null, proposals: [] })),
    aiDecide: vi.fn(async () => ({ id: 1, kind: "assume", text: "1", rationale: "", state: "applied" })),
    uiState: vi.fn(async () => {}),
    aiGuiDone: vi.fn(async () => {}),
    popOutWindow: vi.fn(async () => {}),
    attachHostContext: vi.fn(async () => {}),
    onProgress: (cb: (p: ProveProgress) => void) => {
      progressListeners.add(cb);
      return () => progressListeners.delete(cb);
    },
    onConsole: () => () => {},
    onEngineLog: () => () => {},
    onAskDelta: () => () => {},
    onAskStep: () => () => {},
    onAskDone: () => () => {},
    onAiActivity: () => () => {},
    onAiGui: () => () => {},
    onMcpToolResult: () => () => {},
  },
}));

import App from "./App";

describe("Layer 3 — Full <App /> Interaction, Async Lockout & Escape Priority", () => {
  beforeEach(() => {
    try {
      window.localStorage?.clear?.();
    } catch {
      // ignore if webstorage disabled
    }
    currentSession = makeBaseSession();
    evalTclImpl = async () => ({ ok: true, result: "", session: currentSession });
    evalTclSpy.mockClear();
    stopSpy.mockClear();
    progressListeners.clear();
    useWorkbenchStore.setState({
      layoutPreset: "triage",
      commandPaletteOpen: false,
      walkthrough: null,
      focusChips: [],
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("locks out budget & effort controls while an RPC is in flight so rapid clicks never race or drop", async () => {
    const user = userEvent.setup();
    const { container } = render(<App />);
    const grid = () => within(container.querySelector("table.grid")!);
    await waitFor(() => expect(grid().getByText("a_max")).toBeTruthy());

    await user.click(screen.getByRole("button", { name: "Proof budget" }));
    const pop = screen.getByRole("dialog", { name: "Proof budget settings" });
    expect(pop).toBeTruthy();

    // Hold the `set_prove_time_limit 5m` RPC mid-flight
    const hold = deferred<Reply>();
    evalTclImpl = () => hold.promise;

    const btn5m = screen.getByRole("button", { name: "5m" });
    const btn50 = screen.getByRole("button", { name: "50" });
    await user.click(btn5m);

    expect(evalTclSpy).toHaveBeenCalledTimes(1);
    expect(evalTclSpy).toHaveBeenLastCalledWith("set_prove_time_limit 5m");

    // While busy=true, clicking `50` or `On` is cleanly disabled at the DOM level
    expect((btn50 as HTMLButtonElement).disabled).toBe(true);
    await user.click(btn50);
    expect(evalTclSpy).toHaveBeenCalledTimes(1);

    // Resolve the in-flight RPC -> controls re-enable cleanly and next click dispatches
    currentSession = { ...currentSession, time_limit: { secs: 300, nanos: 0 } };
    evalTclImpl = async (cmd) => {
      if (cmd === "set_max_trace_length 50") {
        currentSession = { ...currentSession, max_trace_length: 50 };
      }
      return { ok: true, result: "", session: currentSession };
    };
    await act(async () => {
      hold.resolve({ ok: true, result: "", session: currentSession });
    });

    await waitFor(() => expect((btn50 as HTMLButtonElement).disabled).toBe(false));
    await user.click(btn50);
    expect(evalTclSpy).toHaveBeenCalledTimes(2);
    expect(evalTclSpy).toHaveBeenLastCalledWith("set_max_trace_length 50");
  });

  it("enforces the full Escape dismissal priority chain before stopping a running prove", async () => {
    const user = userEvent.setup();
    const { container } = render(<App />);
    const grid = () => within(container.querySelector("table.grid")!);
    await waitFor(() => expect(grid().getByText("a_never9")).toBeTruthy());

    // 1. Open BudgetPopover first and verify Escape closes it
    await user.click(screen.getByRole("button", { name: "Proof budget" }));
    expect(screen.getByRole("dialog", { name: "Proof budget settings" })).toBeTruthy();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Proof budget settings" })).toBeNull();

    // 2. Start an in-flight prove (`running = true`)
    const proveHold = deferred<Reply>();
    evalTclImpl = () => proveHold.promise;
    await user.click(screen.getByRole("button", { name: "Prove all" }));
    await act(async () => {
      for (const cb of progressListeners) {
        cb({
          kind: "update",
          total: 3,
          done: 1,
          elapsed_ms: 400,
          phase: "sweep",
          props: [
            { name: "a_max", kind: "assert", status: "proven", engine: "ric3-ic3", ms: 20 },
            { name: "a_never9", kind: "assert", status: "running", engine: "ric3-bmc", depth: 6, ms: 200 },
          ],
        });
      }
    });
    expect(screen.getAllByText(/Pass 1 sweep/).length).toBeGreaterThanOrEqual(1);

    // Open ShortcutSheet via `?` -> Escape closes ShortcutSheet WITHOUT calling stop()
    await user.keyboard("?");
    const guide = screen.getByRole("dialog", { name: "Keyboard & Formal Workflow Guide" });
    expect(guide).toBeTruthy();
    fireEvent.keyDown(guide, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Keyboard & Formal Workflow Guide" })).toBeNull();
    expect(stopSpy).not.toHaveBeenCalled();

    // Open row ContextMenu via right-click -> Escape closes ContextMenu WITHOUT calling stop()
    fireEvent.contextMenu(grid().getByText("a_max"));
    expect(screen.getByRole("menu", { name: "Actions for a_max" })).toBeTruthy();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu", { name: "Actions for a_max" })).toBeNull();
    expect(stopSpy).not.toHaveBeenCalled();

    // Focus filter and type a query -> Escape clears filter WITHOUT calling stop()
    const filterInput = screen.getByRole("textbox", { name: "Filter properties" }) as HTMLInputElement;
    await user.click(filterInput);
    await user.type(filterInput, "max");
    expect(filterInput.value).toBe("max");
    await user.keyboard("{Escape}");
    expect(filterInput.value).toBe("");
    expect(stopSpy).not.toHaveBeenCalled();

    // With all overlays closed and filter empty, Escape NOW stops the running prove!
    await user.keyboard("{Escape}");
    expect(stopSpy).toHaveBeenCalledTimes(1);

    await act(async () => {
      proveHold.resolve({ ok: true, result: "", session: currentSession });
    });
  });

  it("preserves table filter & selection across F3 Source pivot and Alt+ArrowLeft back navigation", async () => {
    const user = userEvent.setup();
    const { container } = render(<App />);
    const grid = () => within(container.querySelector("table.grid")!);
    await waitFor(() => expect(grid().getByText("a_vac")).toBeTruthy());

    // Vacuous proof badge renders on `a_vac` in both PropTable and TaskTree
    expect(screen.getAllByLabelText("vacuous proof").length).toBeGreaterThanOrEqual(2);

    // Filter to Proven via strip summary pill, then select `a_vac`
    await user.click(screen.getByRole("button", { name: "2 proven" }));
    expect(screen.getByRole("button", { name: "clear status filter" })).toBeTruthy();
    await user.click(grid().getByText("a_vac"));

    // Press F3 -> pivots to Source view with ContextBar showing `a_vac` and `⚠ vacuous`
    await user.keyboard("{F3}");
    const ctxBar = screen.getByRole("region", { name: "Active property context" });
    expect(ctxBar.textContent).toContain("a_vac");
    expect(ctxBar.textContent).toContain("vacuous");

    // Press Alt+ArrowLeft -> pops NavSnapshot, returning to Table with `Proven` filter & `a_vac` selected!
    fireEvent.keyDown(window, { key: "ArrowLeft", altKey: true });
    expect(screen.queryByRole("region", { name: "Active property context" })).toBeNull();
    expect(screen.getByRole("button", { name: "clear status filter" }).textContent).toContain("Status: Proven");
    const vacRow = grid().getByText("a_vac").closest("tr");
    expect(vacRow?.getAttribute("aria-selected")).toBe("true");
  });
});
