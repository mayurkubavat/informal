import { describe, expect, it } from "vitest";
import {
  bound, groupOf, identifiers, matches, KIND_LETTER, locate, parsePorts, sortValue, STATUS_LABEL, STATUS_ORDER, SUMMARY, TASK, time,
  statusKey, statusText, clock, isProveCmd, withProgress,
} from "./model";
import type { ProveProgress } from "./backend";
import { mockSource } from "./mock";
import { prop } from "./test/fixtures";

describe("constants", () => {
  it("labels every status word", () => {
    expect(Object.keys(STATUS_LABEL).sort()).toEqual(["cex", "covered", "error", "proven", "running", "undetermined", "unprocessed", "unreachable"]);
    expect(STATUS_LABEL.cex).toBe("CEX");
  });
  it("orders failures first and lists every status once", () => {
    expect(STATUS_ORDER[0]).toBe("error");
    expect(new Set(STATUS_ORDER).size).toBe(8);
    expect(SUMMARY).not.toContain("unprocessed");
  });
  it("uses JasperGold kind letters and the embedded task", () => {
    expect(KIND_LETTER).toEqual({ assert: "A", cover: "C", assume: "M" });
    expect(TASK).toBe("<embedded>");
  });
});

describe("bound", () => {
  it("shows depth for cex and covered", () => {
    expect(bound(prop("p", {}, { status: "cex", depth: 11 }))).toBe("11");
    expect(bound(prop("p", {}, { status: "covered", depth: 3 }))).toBe("3");
    expect(bound(prop("p", {}, { status: "cex" }))).toBe("?");
    expect(bound(prop("p", {}, { status: "cex", depth: null }))).toBe("?");
  });
  it("shows infinity for full proofs", () => {
    expect(bound(prop("p", {}, { status: "proven" }))).toBe("∞");
    expect(bound(prop("p", {}, { status: "unreachable" }))).toBe("∞");
  });
  it("shows the reason or first error line", () => {
    expect(bound(prop("p", {}, { status: "undetermined", reason: "timeout" }))).toBe("timeout");
    expect(bound(prop("p", {}, { status: "undetermined" }))).toBe("");
    expect(bound(prop("p", {}, { status: "error", message: "bad sva\nat line 3" }))).toBe("bad sva");
    expect(bound(prop("p", {}, { status: "error" }))).toBe("");
  });
  it("is empty when unprocessed", () => {
    expect(bound(prop("p"))).toBe("");
  });
});

describe("time", () => {
  it("formats milliseconds as seconds with two decimals", () => {
    expect(time(prop("p", {}, { status: "proven", ms: 41 }))).toBe("0.04s");
    expect(time(prop("p", {}, { status: "proven", ms: 12345 }))).toBe("12.35s");
    expect(time(prop("p", {}, { status: "proven", ms: 0 }))).toBe("0.00s");
  });
  it("is empty without a time", () => {
    expect(time(prop("p"))).toBe("");
  });
});

describe("statusKey / statusText", () => {
  it("reports assumes as assumed whatever their status", () => {
    const m = prop("m", { kind: "assume" }, { status: "proven" });
    expect(statusKey(m)).toBe("assume");
    expect(statusText(m)).toBe("Assumed");
  });
  it("reports disabled properties as unprocessed / Disabled", () => {
    const p = prop("p", { enabled: false }, { status: "cex" });
    expect(statusKey(p)).toBe("unprocessed");
    expect(statusText(p)).toBe("Disabled");
  });
  it("passes the status through otherwise", () => {
    const p = prop("p", {}, { status: "cex" });
    expect(statusKey(p)).toBe("cex");
    expect(statusText(p)).toBe("CEX");
  });
});

describe("sortValue", () => {
  const cex = prop("b_cex", { kind: "cover" }, { status: "cex", engine: "bmc", depth: 4, ms: 10 });
  const pr = prop("a_pr", {}, { status: "proven", engine: "ic3", ms: 50 });
  const un = prop("c_un");
  const m = prop("m", { kind: "assume" });
  it("sorts by kind, name and engine as strings", () => {
    expect(sortValue(cex, "kind")).toBe("cover");
    expect(sortValue(pr, "name")).toBe("a_pr");
    expect(sortValue(pr, "engine")).toBe("ic3");
    expect(sortValue(un, "engine")).toBe("");
  });
  it("sorts status by severity with assumes last", () => {
    expect(sortValue(cex, "status")).toBeLessThan(sortValue(pr, "status") as number);
    expect(sortValue(pr, "status")).toBeLessThan(sortValue(un, "status") as number);
    expect(sortValue(m, "status")).toBe(99);
  });
  it("sorts bound with full proofs after any depth and unknown first", () => {
    expect(sortValue(cex, "bound")).toBe(4);
    expect(sortValue(pr, "bound")).toBe(1e9);
    expect(sortValue(un, "bound")).toBe(-1);
  });
  it("sorts time with missing times first", () => {
    expect(sortValue(pr, "time")).toBe(50);
    expect(sortValue(un, "time")).toBe(-1);
  });
});

describe("identifiers", () => {
  it("drops SVA system-function names, keywords and duplicates", () => {
    expect(identifiers("en && !done |=> q == $past(q) + 4'd1")).toEqual(["en", "done", "q", "d1"]);
    expect(identifiers("@(posedge clk) disable iff (rst) $rose(req)")).toEqual(["clk", "rst", "req"]);
    expect(identifiers("1 + 2")).toEqual([]);
  });
});

describe("locate", () => {
  const src = "module m;\n  logic q;\n  a_named: assert property (q);\nendmodule";
  it("prefers the line naming the property", () => {
    expect(locate(src, prop("a_named", { expr: "zzz" }))).toBe(3);
  });
  it("falls back to the first line using one of its signals", () => {
    expect(locate(src, prop("other", { expr: "q != 0" }))).toBe(2);
    expect(locate(mockSource, prop("a_never9", { expr: "q != 4'd9" }))).toBe(2);
  });
  it("returns 0 when nothing matches or there is no input", () => {
    expect(locate(src, prop("x", { expr: "nope" }))).toBe(0);
    expect(locate(src, prop("x", { expr: "1" }))).toBe(0);
    expect(locate("", prop("a_named"))).toBe(0);
    expect(locate(src, null)).toBe(0);
  });
});

describe("parsePorts", () => {
  it("reads an ANSI port list, carrying direction and width forward", () => {
    expect(parsePorts(mockSource)).toEqual([
      { dir: "input", name: "clk", width: "" },
      { dir: "input", name: "rst_n", width: "" },
      { dir: "input", name: "en", width: "" },
      { dir: "output", name: "q", width: "[3:0]" },
      { dir: "output", name: "done", width: "" },
    ]);
  });
  it("handles parameters, signed, inout and shared declarations", () => {
    const src = "module m #(parameter W = 8) (input wire signed [W-1:0] a, b, inout c, output reg [1:0] d);";
    expect(parsePorts(src)).toEqual([
      { dir: "input", name: "a", width: "[W-1:0]" },
      { dir: "input", name: "b", width: "[W-1:0]" },
      { dir: "inout", name: "c", width: "" },
      { dir: "output", name: "d", width: "[1:0]" },
    ]);
  });
  it("returns nothing without a module header or for non-ANSI ports", () => {
    expect(parsePorts("")).toEqual([]);
    expect(parsePorts("module m (a, b);")).toEqual([]);
  });
});

describe("groupOf", () => {
  it("groups assumes together and others by raw status, even when disabled", () => {
    expect(groupOf(prop("m", { kind: "assume" }, { status: "proven" }))).toBe("assume");
    expect(groupOf(prop("p", { enabled: false }, { status: "cex" }))).toBe("cex");
    expect(groupOf(prop("p"))).toBe("unprocessed");
  });
});

describe("matches", () => {
  const p = prop("a_Never9", { expr: "q != 4'd9" }, { status: "cex", engine: "ric3-BMC" });
  it("matches name, expression and engine, ignoring case and surrounding space", () => {
    expect(matches(p, "never")).toBe(true);
    expect(matches(p, "Q != 4")).toBe(true);
    expect(matches(p, "bmc")).toBe(true);
    expect(matches(p, "  A_NEVER9 ")).toBe(true);
  });
  it("matches everything for an empty query and nothing for a miss", () => {
    expect(matches(p, "")).toBe(true);
    expect(matches(p, "   ")).toBe(true);
    expect(matches(p, "kind")).toBe(false);
    expect(matches(prop("x"), "bmc")).toBe(false);
  });
});

describe("running view status", () => {
  const run = prop("p", {}, { status: "running", engine: "ric3-bmc", depth: 16, ms: 65_400 } as never);
  it("labels, glyph key, bound and time", () => {
    expect(STATUS_LABEL.running).toBe("Running");
    expect(statusText(run)).toBe("Running");
    expect(statusKey(run)).toBe("running");
    expect(groupOf(run)).toBe("running");
    expect(bound(run)).toBe("k=16");
    expect(bound(prop("q", {}, { status: "running" } as never))).toBe("k=0");
    expect(time(run)).toBe("1:05");
  });
  it("sorts after failures and before undetermined", () => {
    expect(STATUS_ORDER.indexOf("running")).toBeGreaterThan(STATUS_ORDER.indexOf("cex"));
    expect(STATUS_ORDER.indexOf("running")).toBeLessThan(STATUS_ORDER.indexOf("undetermined"));
    expect(SUMMARY).not.toContain("running");
  });
});

describe("clock", () => {
  it("formats m:ss and floors partial seconds", () => {
    expect(clock(0)).toBe("0:00");
    expect(clock(999)).toBe("0:00");
    expect(clock(61_000)).toBe("1:01");
    expect(clock(600_000)).toBe("10:00");
    expect(clock(-5)).toBe("0:00");
  });
});

describe("isProveCmd", () => {
  it("recognises prove and check_fv only as the command word", () => {
    expect(isProveCmd("prove -all")).toBe(true);
    expect(isProveCmd("  check_fv -property {a}")).toBe(true);
    expect(isProveCmd("prove")).toBe(true);
    expect(isProveCmd("prover")).toBe(false);
    expect(isProveCmd("get_prove_result")).toBe(false);
    expect(isProveCmd("puts prove")).toBe(false);
  });
});

describe("withProgress", () => {
  const props = [prop("a", {}, { status: "proven", engine: "old", ms: 1 }), prop("b"), prop("c", {}, { status: "cex", depth: 3 })];
  const prog: ProveProgress = {
    kind: "update",
    total: 2,
    done: 1,
    elapsed_ms: 900,
    props: [
      { name: "a", status: "running", engine: "ric3", depth: 6, ms: 300 },
      { name: "b", status: "undetermined", reason: "stopped" },
    ],
  };
  it("overlays run members and leaves the rest alone", () => {
    const v = withProgress(props, prog);
    expect(v[0].status).toEqual({ status: "running", engine: "ric3", depth: 6, ms: 300 });
    expect(v[1].status).toEqual({ status: "undetermined", reason: "stopped" });
    expect(bound(v[1])).toBe("stopped");
    expect(v[2]).toBe(props[2]);
    expect(v[0].expr).toBe(props[0].expr);
  });
  it("returns the session's properties untouched without progress", () => {
    expect(withProgress(props, null)).toBe(props);
  });
});
