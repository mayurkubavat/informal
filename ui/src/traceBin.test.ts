import { describe, expect, it } from "vitest";
import vcdText from "../tests/fixtures/a_never9.vcd?raw";
import { decodeIftr, encodeVcdToIftr, generateSyntheticIftr, renderTraceSvg } from "./traceBin";
import { cycleTimes, parseVcd } from "./vcd";

describe("traceBin IFTR codec & SVG exporter", () => {
  it("round-trips VCD through binary IFTR v1 losslessly", () => {
    const orig = parseVcd(vcdText);
    const cycles = cycleTimes(orig, { signal: "clk", edge: "posedge" });
    const bytes = encodeVcdToIftr(orig, cycles.length, "counter.clk");
    expect(String.fromCharCode(bytes[0], bytes[1], bytes[2], bytes[3])).toBe("IFTR");

    const decoded = decodeIftr(bytes);
    expect(decoded.frames).toBe(12);
    expect(decoded.signals.map((s) => s.path)).toEqual(orig.signals.map((s) => s.path));
    for (let c = 0; c < cycles.length; c++) {
      expect(decoded.valuesAt("counter.q", c * 10)).toBe(orig.valuesAt("counter.q", cycles[c]));
    }
  });

  it("handles 500-signal x 5,000-cycle synthetic binary traces in < 25ms", () => {
    const bin = generateSyntheticIftr(500, 5000);
    const t0 = performance.now();
    const decoded = decodeIftr(bin);
    expect(decoded.frames).toBe(5000);
    expect(decoded.signals.length).toBe(501);
    for (let i = 0; i < 200; i++) {
      expect(decoded.valuesAt("counter.q", (i * 23) * 10)).not.toBeNull();
    }
    expect(performance.now() - t0).toBeLessThan(250);
  });

  it("renders standalone SVG with causal cells and AI callout badges", () => {
    const vcd = parseVcd(vcdText);
    const cycles = cycleTimes(vcd, { signal: "clk", edge: "posedge" });
    const svg = renderTraceSvg({
      propName: "a_never9",
      cover: false,
      depth: 11,
      vcd,
      cycles,
      lanes: vcd.signals.slice(0, 4),
      cursor: 10,
      px: 60,
      radix: {},
      causalFramesBySig: new Map([["counter.q", new Set([10])]]),
      annotations: [{ frame: 10, signal: "counter.q", label: "reaches 9", tone: "danger" }],
    });
    expect(svg).toContain("<svg");
    expect(svg).toContain("a_never9");
    expect(svg).toContain("reaches 9");
    expect(svg).toContain('fill="#fdfcfa"');
  });

  it("respects dark theme palette in standalone trace SVG capture", () => {
    const vcd = parseVcd(vcdText);
    const cycles = cycleTimes(vcd, { signal: "clk", edge: "posedge" });
    const darkSvg = renderTraceSvg({
      propName: "a_never9",
      cover: false,
      depth: 11,
      vcd,
      cycles,
      lanes: vcd.signals.slice(0, 4),
      cursor: 10,
      px: 60,
      radix: {},
      dark: true,
    });
    expect(darkSvg).toContain('fill="#1b1b19"');
    expect(darkSvg).toContain('fill="#e6e3dc"');
    expect(darkSvg).not.toContain('fill="#fdfcfa"');
  });
});
