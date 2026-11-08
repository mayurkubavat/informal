import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MOCK_ASK_REPLY, MOCK_ENGINES, mockAsk, mockAskCancel, mockLlmProviders, mockLlmSelect, mockLlmStatus, mockOpenInSurfer, mockSurferOpened, mock, mockComplete, mockEvents, mockOptions, mockRunning, PROVE_ON_SOURCE, resetMock, TICK_MS } from "./mock";
import type { ConsoleLine, EngineLine, ProveProgress } from "./backend";

// The mock is module state shared across tests, so each test leaves it as
// it found it where that matters.
const status = async (name: string) => (await mock("snapshot", {})).session!.props.find((p) => p.name === name)!;

describe("browser mock", () => {
  beforeEach(() => {
    resetMock();
    mockOptions.tick = 0; // the run finishes inside the call
  });

  it("snapshots the counter design with six properties", async () => {
    const r = await mock("snapshot", {});
    expect(r.ok).toBe(true);
    expect(r.session?.design.top).toBe("counter");
    expect(r.session?.props.map((p) => p.name)).toEqual(["a_max", "a_step", "a_never9", "c_done", "c_10", "m_en"]);
  });

  it("completes the flavor's commands, then property names", async () => {
    expect(mockComplete("pro")).toEqual(["prove"]);
    expect(mockComplete("a_")).toEqual(["a_max", "a_never9", "a_step"]);
    expect(mockComplete("")).toEqual([]);
    await mock("shell_flavor", { flavor: "vcf" });
    expect(mockComplete("fv")).toEqual(["fvassert", "fvassume", "fvcover", "fvdisable", "fvenable"]);
    expect(mockComplete("pro")).toEqual([]);
  });

  it("emits engine-log lines while proving", async () => {
    const got: EngineLine[] = [];
    const off = mockEvents.on("engine-log", (l) => got.push(l as EngineLine));
    await mock("shell_eval", { script: "prove -property a_never9" });
    off();
    expect(got.map((l) => l.text)).toEqual(["reading fdv_work/a_never9.btor2", "frame 6", "sat"]);
    expect(got.every((l) => l.prop === "a_never9" && l.engine === "ric3-bmc" && !l.err)).toBe(true);
  });

  it("refuses fvdisable under the JasperGold flavor", async () => {
    const r = await mock("shell_eval", { script: "fvdisable {a_max}" });
    expect(r.ok).toBe(false);
    expect((await status("a_max")).enabled).toBe(true);
  });

  it("toggles properties with fvdisable/fvenable under VC Formal", async () => {
    await mock("shell_flavor", { flavor: "vcf" });
    expect((await mock("shell_eval", { script: "fvdisable {a_max a_step}" })).ok).toBe(true);
    expect((await status("a_max")).enabled).toBe(false);
    expect((await status("a_step")).enabled).toBe(false);
    expect((await mock("shell_eval", { script: "fvdisable nope" })).result).toMatch(/no property matches/);
    await mock("shell_eval", { script: "fvenable {a_max a_step}" });
    expect((await status("a_max")).enabled).toBe(true);
    await mock("shell_flavor", { flavor: "jg" });
  });

  it("proves one property with -property, then all", async () => {
    const one = await mock("shell_eval", { script: "prove -property c_10" });
    expect(one.result).toBe("proven 0 cex 0 covered 0 unreachable 1 undetermined 0 error 0");
    expect((await status("a_max")).status.status).toBe("unprocessed");
    const vcf = await mock("shell_eval", { script: "check_fv -property {a_never9}" });
    expect(vcf.result).toContain("cex 1");
    const all = await mock("shell_eval", { script: "prove -all" });
    expect(all.result).toBe("proven 2 cex 1 covered 1 unreachable 1 undetermined 0 error 0");
  });

  it("answers other Tcl with the browser-preview error", async () => {
    const r = await mock("shell_eval", { script: "get_design_info" });
    expect(r).toMatchObject({ ok: false, result: "browser preview: Tcl runs only in the desktop app" });
  });
});

describe("browser mock: live progress and stop", () => {
  let events: ProveProgress[];
  let lines: ConsoleLine[];
  let offs: (() => void)[];
  const running = (e: ProveProgress) => e.props.filter((p) => p.status === "running").map((p) => p.name);
  beforeEach(() => {
    resetMock();
    mockOptions.tick = 5;
    events = [];
    lines = [];
    offs = [
      mockEvents.on("prove-progress", (e) => events.push(e as ProveProgress)),
      mockEvents.on("console-line", (l) => lines.push(l as ConsoleLine)),
    ];
  });
  afterEach(() => {
    offs.forEach((off) => off());
    resetMock();
  });

  it("emits start, one update per tick, a line per finished property, then done and the reply", async () => {
    const r = await mock("shell_eval", { script: "prove -all" });
    expect(r.result).toBe("proven 2 cex 1 covered 1 unreachable 1 undetermined 0 error 0");
    expect(events.map((e) => e.kind)).toEqual(["start", "update", "update", "update", "update", "done"]);
    expect(events[0]).toMatchObject({ total: 5, done: 0, elapsed_ms: 0 });
    expect(running(events[0])).toEqual(["a_max", "a_step"]);
    for (const e of events) expect(running(e).length).toBeLessThanOrEqual(2);
    expect(events.map((e) => e.elapsed_ms)).toEqual(events.map((_, i) => i * TICK_MS));
    expect(events.map((e) => e.done)).toEqual([0, 0, 1, 2, 3, 5]);
    const step = events.map((e) => e.props.find((p) => p.name === "a_step")!).filter((p) => p.status === "running");
    expect(step.map((p) => p.depth)).toEqual([1, 6, 11]);
    expect(lines.map((l) => l.text)).toEqual([
      "a_max: proven [ric3-ic3, 41 ms]",
      "a_step: proven [ric3, 55 ms]",
      "a_never9: cex at depth 11 [ric3-bmc, 30 ms]",
      "c_done: covered at depth 11 [ric3-bmc, 31 ms]",
      "c_10: unreachable [ric3-ic3, 29 ms]",
    ]);
    expect(mockRunning()).toBe(false);
  });

  it("finishes inside the call in fast mode, still with start and done", async () => {
    mockOptions.tick = 0;
    await mock("shell_eval", { script: "prove -property c_10" });
    expect(events.map((e) => e.kind)).toEqual(["start", "done"]);
    expect(events[1].props).toEqual([{ name: "c_10", kind: "cover", status: "unreachable", engine: "ric3-ic3", ms: 29, engines: MOCK_ENGINES.c_10 }]);
    // The session's property carries the same portfolio record.
    expect((await mock("snapshot", {})).session!.props.find((p) => p.name === "c_10")!.engines).toEqual(MOCK_ENGINES.c_10);
  });

  it("stops mid-run: unfinished properties become undetermined (stopped)", async () => {
    mockOptions.freeze = 2;
    const pending = mock("shell_eval", { script: "prove -all" });
    await vi.waitFor(() => expect(events).toHaveLength(3));
    expect(running(events[2])).toEqual(["a_step", "a_never9"]);
    expect(mockRunning()).toBe(true);
    const again = await mock("shell_eval", { script: "check_fv" });
    expect(again).toMatchObject({ ok: false, result: "a prove is already running; stop it first" });
    expect((await mock("shell_stop", {})).ok).toBe(true);
    const r = await pending;
    expect(r.result).toBe("proven 1 cex 0 covered 0 unreachable 0 undetermined 4 error 0");
    const last = events[events.length - 1];
    expect(last).toMatchObject({ kind: "stopped", done: 5, total: 5 });
    expect(last.props.find((p) => p.name === "a_step")).toEqual({
      name: "a_step", kind: "assert", status: "undetermined", reason: "stopped", engine: "ric3", ms: 2 * TICK_MS,
    });
    expect(last.props.find((p) => p.name === "c_10")).toEqual({ name: "c_10", kind: "cover", status: "undetermined", reason: "stopped" });
    expect(lines[lines.length - 1]).toEqual({ kind: "err", text: "prove stopped: 4 properties left undetermined" });
    expect((await status("a_step")).status).toMatchObject({ status: "undetermined", reason: "stopped" });
    expect((await status("a_max")).status.status).toBe("proven");
    expect(mockRunning()).toBe(false);
  });

  it("names each property's kind in every event", async () => {
    await mock("shell_eval", { script: "prove -all" });
    expect(events[0].props.map((p) => [p.name, p.kind])).toEqual([
      ["a_max", "assert"], ["a_step", "assert"], ["a_never9", "assert"], ["c_done", "cover"], ["c_10", "cover"],
    ]);
    for (const e of events) expect(e.props.every((p) => p.kind)).toBe(true);
  });

  it("source prove-on-source.tcl proves properties the session gets only with the reply", async () => {
    mockOptions.freeze = 1;
    const pending = mock("shell_source", { path: PROVE_ON_SOURCE });
    await vi.waitFor(() => expect(events).toHaveLength(2));
    expect(events[0].props.map((p) => [p.name, p.kind, p.status])).toEqual([
      ["a_src_ok", "assert", "running"], ["c_src_hit", "cover", "running"],
    ]);
    expect((await mock("snapshot", {})).session!.props).toHaveLength(6);
    await mock("shell_stop", {});
    const r = await pending;
    expect(r.session!.props.map((p) => p.name).slice(6)).toEqual(["a_src_ok", "c_src_hit"]);
    expect(r.session!.props[7]).toMatchObject({ kind: "cover", status: { status: "undetermined", reason: "stopped" } });
    resetMock();
    expect((await mock("snapshot", {})).session!.props).toHaveLength(6);
  });

  it("answers shell_stop when idle without events", async () => {
    expect(await mock("shell_stop", {})).toMatchObject({ ok: true, result: "" });
    expect(events).toEqual([]);
  });

  it("holds the stop under ?holdStop so the run stays up", async () => {
    Object.assign(mockOptions, { freeze: 1, holdStop: true });
    void mock("shell_eval", { script: "prove -all" });
    await vi.waitFor(() => expect(events).toHaveLength(2));
    let answered = false;
    void mock("shell_stop", {}).then(() => (answered = true));
    await new Promise((r) => setTimeout(r, 250));
    expect(answered).toBe(false);
    expect(mockRunning()).toBe(true);
  });
});

describe("browser mock: Surfer hand-off", () => {
  afterEach(() => resetMock());
  it("is unavailable by default and refuses to open", async () => {
    const { backend } = await import("./backend");
    expect(await backend.toolAvailable("surfer")).toBe(false);
    await expect(mockOpenInSurfer("fdv_work/a_never9.vcd")).rejects.toThrow(/surfer is not on PATH/);
  });
  it("with ?surfer=1 opens known traces only", async () => {
    mockOptions.surfer = true;
    const { backend } = await import("./backend");
    expect(await backend.toolAvailable("surfer")).toBe(true);
    expect(await backend.toolAvailable("gtkwave")).toBe(false);
    await mockOpenInSurfer("fdv_work/a_never9.vcd");
    await expect(mockOpenInSurfer("fdv_work/prop_0.btor2")).rejects.toThrow(/not a trace/);
    expect(mockSurferOpened).toEqual(["fdv_work/a_never9.vcd"]);
  });
});

describe("browser mock: Ask mode (Fake provider)", () => {
  beforeEach(() => resetMock());

  const collect = (id: number) =>
    new Promise<{ text: string; done: import("./ask").AskDone }>((resolve) => {
      let text = "";
      const offD = mockEvents.on("ask-delta", (d) => {
        const e = d as import("./ask").AskDelta;
        if (e.id === id) text += e.text;
      });
      const offE = mockEvents.on("ask-done", (d) => {
        const e = d as import("./ask").AskDone;
        if (e.id !== id) return;
        offD();
        offE();
        resolve({ text, done: e });
      });
    });

  it("streams the scripted answer and suggests prove -property a_never9", async () => {
    mockOptions.tick = 0;
    expect(mockLlmStatus()).toEqual({ provider: "fake", model: "scripted", ok: true });
    const got = collect(7);
    expect(mockAsk(7, "prove only the a_never9 property")).toBe(7);
    const { text, done } = await got;
    expect(text).toBe(MOCK_ASK_REPLY);
    expect(done.command).toBe("prove -property a_never9");
    expect(done.explanation).toBe("Proving only that property:");
  });

  it("answers which properties failed from the session, with no command", async () => {
    mockOptions.tick = 0;
    await mock("shell_eval", { script: "prove -all" });
    const got = collect(8);
    mockAsk(8, "which properties failed?");
    const { done } = await got;
    expect(done.command ?? null).toBeNull();
    expect(done.explanation).toBe("One property failed: a_never9 (CEX).");
  });

  it("cancels a streaming answer with one cancelled ask-done", async () => {
    mockOptions.tick = 300;
    const got = collect(9);
    mockAsk(9, "prove it");
    expect(mockAskCancel(9)).toBe(true);
    expect((await got).done.error).toBe("cancelled");
    expect(mockAskCancel(9)).toBe(false);
  });

  it("lists two providers; ollama-gemma is offline and ask then refuses", () => {
    expect(mockLlmProviders().map((p) => [p.name, p.selected])).toEqual([["fake", true], ["ollama-gemma", false]]);
    const st = mockLlmSelect("ollama-gemma");
    expect(st.ok).toBe(false);
    expect(st.error).toContain("Connection refused");
    expect(() => mockAsk(1, "x")).toThrow(/Connection refused/);
    expect(() => mockLlmSelect("nope")).toThrow();
    mockLlmSelect("fake");
    mockOptions.llmOffline = true;
    expect(mockLlmStatus().ok).toBe(false);
  });
});
