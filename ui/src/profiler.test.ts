import { describe, expect, it } from "vitest";
import { barShare, msText, profileSummary, withProgress } from "./model";
import type { EngineRun } from "./backend";
import { MOCK_ENGINES } from "./mock";
import { prop } from "./test/fixtures";

const r = (engine: string, outcome: EngineRun["outcome"], ms: number): EngineRun => ({ engine, outcome, ms });

describe("proof profiler aggregation", () => {
  it("counts runs and wins, sums time, and averages time-to-win over wins only", () => {
    const s = profileSummary([
      { engines: [r("ic3", "won", 40), r("bmc", "killed", 41)] },
      { engines: [r("ic3", "killed", 30), r("bmc", "won", 20)] },
      { engines: [r("ic3", "won", 60), r("bmc", "timeout", 100), r("kind", "error", 2)] },
      { engines: [] },
      {},
    ]);
    expect(s).toEqual([
      { engine: "ic3", runs: 3, wins: 2, totalMs: 130, meanWinMs: 50 },
      { engine: "bmc", runs: 3, wins: 1, totalMs: 161, meanWinMs: 20 },
      { engine: "kind", runs: 1, wins: 0, totalMs: 2, meanWinMs: null },
    ]);
  });
  it("orders ties by less total time, then by name", () => {
    const s = profileSummary([{ engines: [r("b", "lost", 5), r("a", "lost", 5), r("c", "lost", 1)] }]);
    expect(s.map((e) => e.engine)).toEqual(["c", "a", "b"]);
    expect(profileSummary([])).toEqual([]);
  });
  it("summarises the mock's full run with ric3-ic3 and ric3-bmc ahead", () => {
    const props = ["a_max", "a_step", "a_never9", "c_done", "c_10"].map((n) => ({ engines: MOCK_ENGINES[n] }));
    const s = profileSummary(props);
    expect(s.map((e) => [e.engine, e.wins])).toEqual([["ric3-bmc", 2], ["ric3-ic3", 2], ["ric3", 1], ["fdv-kind", 0]]);
    expect(s.find((e) => e.engine === "ric3-ic3")!.meanWinMs).toBe(35);
  });
  it("scales bars to the longest time and formats times", () => {
    expect(barShare(50, 200)).toBe(0.25);
    expect(barShare(300, 200)).toBe(1);
    expect(barShare(5, 0)).toBe(0);
    expect(msText(41)).toBe("41 ms");
    expect(msText(1234)).toBe("1.23 s");
  });
  it("takes a finished property's record from the run's progress, not its status", () => {
    const runs = [r("ic3", "won", 3)];
    const v = withProgress([prop("a", { engines: [r("old", "won", 9)] })], {
      kind: "done", total: 1, done: 1, elapsed_ms: 5,
      props: [{ name: "a", kind: "assert", status: "proven", engine: "ic3", ms: 3, engines: runs }],
    });
    expect(v[0].engines).toEqual(runs);
    expect(v[0].status).toEqual({ status: "proven", engine: "ic3", ms: 3 });
  });
});
