import { beforeEach, describe, expect, it } from "vitest";
import { mock, mockOptions, resetMock } from "./mock";

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
