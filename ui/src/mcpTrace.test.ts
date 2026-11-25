import { describe, expect, it } from "vitest";
import { mcpTraceToVcdText, traceValueToBits } from "./mcpTrace";
import { cycleTimes, formatValue, parseVcd } from "./vcd";

describe("mcpTraceToVcdText", () => {
  it("converts informal.trace/1 hex & single-bit signals (including lp.pd.* swimlanes) into valid VCD", () => {
    const vcdText = mcpTraceToVcdText({
      schema: "informal.trace/1",
      frames: 3,
      unit: "cycle",
      hit_at: [2],
      property: { name: "LowPower::lp_iso_before_pwroff_PD_TIM" },
      signals: [
        { name: "apb_pmu_timer_subsys.u_pmu.tim_pwr_en_q", width: 1, values: ["1", "1", "0"] },
        { name: "apb_pmu_timer_subsys.PRDATA", width: 32, values: ["00000011", "00000011", "00008000"] },
        { name: "lp.pd.PD_TIM.on", width: 1, values: ["1", "1", "0"] },
        { name: "lp.pd.PD_TIM.corrupt", width: 1, values: ["0", "0", "1"] },
      ],
    });

    const vcd = parseVcd(vcdText);
    const cycles = cycleTimes(vcd);
    expect(cycles).toEqual([0, 10, 20]);

    const pwr = vcd.signals.find((s) => s.path === "apb_pmu_timer_subsys.u_pmu.tim_pwr_en_q");
    const prdata = vcd.signals.find((s) => s.path === "apb_pmu_timer_subsys.PRDATA");
    const lpCorrupt = vcd.signals.find((s) => s.path === "lp.pd.PD_TIM.corrupt");
    expect(pwr).toBeDefined();
    expect(prdata).toBeDefined();
    expect(lpCorrupt).toBeDefined();

    expect(vcd.valuesAt(pwr!, 0)).toBe("1");
    expect(vcd.valuesAt(pwr!, 20)).toBe("0");
    expect(formatValue(vcd.valuesAt(prdata!, 0), "hex")).toBe("11");
    expect(formatValue(vcd.valuesAt(prdata!, 20), "hex")).toBe("8000");
    expect(vcd.valuesAt(lpCorrupt!, 20)).toBe("1");
  });

  it("pads and truncates hex values cleanly including X/Z digits", () => {
    expect(traceValueToBits("1", 1)).toBe("1");
    expect(traceValueToBits("a", 4)).toBe("1010");
    expect(traceValueToBits("1f", 5)).toBe("11111");
    expect(traceValueToBits("x", 4)).toBe("xxxx");
  });
});
