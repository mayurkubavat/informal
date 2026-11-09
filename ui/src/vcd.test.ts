import { describe, expect, it } from "vitest";
import counter from "../tests/fixtures/a_never9.vcd?raw";
import { cycleTimes, formatValue, parseVcd } from "./vcd";

describe("parseVcd on the counter fixture", () => {
  const vcd = parseVcd(counter);

  it("reads the header", () => {
    expect(vcd.timescale).toBe("1ns");
    expect(vcd.signals.map((s) => [s.path, s.width])).toEqual([
      ["counter.clk", 1],
      ["counter.rst_n", 1],
      ["counter.en", 1],
      ["counter.q", 4],
      ["counter.done", 1],
      ["informal.informal_cyc", 16],
    ]);
    expect(vcd.signals[3].name).toBe("q");
  });

  it("has 12 cycles from clk's rising edges", () => {
    expect(cycleTimes(vcd)).toEqual([0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110]);
    expect(vcd.times[0]).toBe(0);
    expect(vcd.times[vcd.times.length - 1]).toBe(115);
  });

  it("gives values in effect at a time", () => {
    const q = (c: number) => parseInt(vcd.valuesAt("counter.q", c * 10)!, 2);
    expect([...Array(12)].map((_, c) => q(c))).toEqual([0, 0, 1, 2, 2, 3, 4, 5, 6, 7, 8, 9]);
    expect(vcd.valuesAt("counter.q", 110)).toBe("1001");
    expect(vcd.valuesAt("counter.q", 115)).toBe("1001"); // between changes
    expect(vcd.valuesAt("counter.done", 100)).toBe("0");
    expect(vcd.valuesAt("counter.done", 110)).toBe("1");
    expect(vcd.valuesAt("counter.rst_n", 0)).toBe("0");
    expect(vcd.valuesAt("counter.en", 30)).toBe("0");
    expect(vcd.valuesAt("counter.clk", 5)).toBe("0");
    expect(vcd.valuesAt("counter.clk", -1)).toBeNull();
    expect(vcd.valuesAt("no.such", 0)).toBeNull();
  });

  it("lists changes only when the value changes", () => {
    expect(vcd.changes("counter.en")).toEqual([
      { t: 0, v: "1" },
      { t: 30, v: "0" },
      { t: 40, v: "1" },
    ]);
  });
});

describe("parseVcd edge cases", () => {
  it("left-extends vectors, keeps x/z, aliases and nested scopes", () => {
    const v = parseVcd(`$timescale 10 ps $end
$scope module top $end $scope module u $end
$var reg 8 a data [7:0] $end
$var wire 8 a alias $end
$upscope $end $upscope $end
$enddefinitions $end
#0 bx a
#1 b101 a
#2 bz a
#3 b1x a`);
    expect(v.timescale).toBe("10ps");
    expect(v.signals.map((s) => s.path)).toEqual(["top.u.data", "top.u.alias"]);
    expect(v.valuesAt("top.u.data", 0)).toBe("xxxxxxxx");
    expect(v.valuesAt("top.u.alias", 1)).toBe("00000101");
    expect(v.valuesAt("top.u.data", 2)).toBe("zzzzzzzz");
    expect(v.valuesAt("top.u.data", 3)).toBe("0000001x");
  });

  it("without a clock, every change time is a cycle", () => {
    const v = parseVcd("$var wire 1 ! a $end $enddefinitions $end #0 0! #4 1! #9 0!");
    expect(cycleTimes(v)).toEqual([0, 4, 9]);
  });
});

describe("formatValue", () => {
  it("formats in bin, dec and hex", () => {
    expect(formatValue("1001", "bin")).toBe("1001");
    expect(formatValue("1001", "dec")).toBe("9");
    expect(formatValue("1001", "hex")).toBe("9");
    expect(formatValue("11111010", "hex")).toBe("fa");
    expect(formatValue("11111010", "dec")).toBe("250");
    expect(formatValue("1" + "0".repeat(64), "hex")).toBe("1" + "0".repeat(16));
  });
  it("keeps unknowns visible", () => {
    expect(formatValue("xxxx", "hex")).toBe("x");
    expect(formatValue("zzzz", "hex")).toBe("z");
    expect(formatValue("10x10001", "hex")).toBe("x1");
    expect(formatValue("10x1", "dec")).toBe("x");
    expect(formatValue("zz", "dec")).toBe("z");
    expect(formatValue("10x1", "bin")).toBe("10x1");
    expect(formatValue(null, "hex")).toBe("");
  });
});
