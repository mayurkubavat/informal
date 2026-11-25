import { describe, expect, it } from "vitest";
import { encodeVcdToIftr, generateSyntheticIftr } from "./traceBin";
import { WindowedTraceStore, type TraceMeta } from "./traceStream";
import { parseVcd } from "./vcd";

describe("WindowedTraceStore", () => {
  it("merges streamed windows and serves exact signal values without loading full trace", () => {
    const meta: TraceMeta = {
      frames: 1000,
      steps: false,
      format: "fst",
      block_count: 4,
      signals: [
        { name: "clk", path: "counter.clk", width: 1, clock: true },
        { name: "q", path: "counter.q", width: 8, clock: false },
        { name: "en", path: "counter.en", width: 1, clock: false },
      ],
    };
    const store = new WindowedTraceStore(meta);
    expect(store.isCovered(0, 100)).toBe(false);
    expect(store.nextMissingChunk(520, 560, 256)).toEqual({ first: 512, last: 767 });

    // Generate synthetic IFTR and merge only the [512..767] window
    const full = generateSyntheticIftr(2, 1000);
    store.mergeWindow(512, 767, full);
    expect(store.isCovered(520, 560)).toBe(true);
    expect(store.nextMissingChunk(520, 560, 256)).toBeNull();
    expect(store.isCovered(0, 100)).toBe(false);

    const vcd = store.toVcd();
    expect(vcd.frames).toBe(1000);
    expect(vcd.format).toBe("fst");
    const qSig = vcd.signals.find((s) => s.path === "counter.q")!;
    // Unloaded region returns null; loaded region returns concrete 8-bit value
    expect(vcd.valuesAt(qSig, 0)).toBeNull();
    expect(vcd.valuesAt(qSig, 520 * 10)).toMatch(/^[01]{8}$/);
  });

  it("coalesces adjacent windows into a single covered range", () => {
    const vcd = parseVcd(
      "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! clk $end\n$var reg 1 \" a $end\n$upscope $end\n$enddefinitions $end\n#0\n1!\n0\"\n#10\n1\"\n#20\n0\"\n#30\n",
    );
    const bin = encodeVcdToIftr(vcd, 3, "t.clk");
    const store = new WindowedTraceStore({
      frames: 3,
      steps: false,
      format: "fst",
      block_count: 1,
      signals: [
        { name: "clk", path: "t.clk", width: 1, clock: true },
        { name: "a", path: "t.a", width: 1, clock: false },
      ],
    });
    store.mergeWindow(0, 1, bin);
    store.mergeWindow(2, 2, bin);
    expect(store.loadedRanges()).toEqual([{ first: 0, last: 2 }]);
    expect(store.isCovered(0, 2)).toBe(true);
    const view = store.toVcd();
    const aSig = view.signals[1];
    expect(view.valuesAt(aSig, 0)).toBe("0");
    expect(view.valuesAt(aSig, 10)).toBe("1");
    expect(view.valuesAt(aSig, 20)).toBe("0");
  });
});
