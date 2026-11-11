import { describe, expect, it } from "vitest";
import { askLines, llmLabel, llmTip, prefill, splitAnswer, tclWord } from "./ask";

describe("splitAnswer", () => {
  it("takes the first tcl block as the command", () => {
    expect(splitAnswer("Proving only that one.\n```tcl\nprove -property a_never9\n```\n")).toEqual({
      explanation: "Proving only that one.",
      command: "prove -property a_never9",
    });
    expect(splitAnswer("```Tcl\nprove -all\n```").command).toBe("prove -all");
    expect(splitAnswer("```\nreport -summary\n```").command).toBe("report -summary");
    expect(splitAnswer("Before\n```tcl\nprove -all\n```\nAfter.").explanation).toBe("Before After.");
  });
  it("gives no command for plain answers, empty or foreign blocks", () => {
    expect(splitAnswer("One property failed: a_never9 (CEX).")).toEqual({ explanation: "One property failed: a_never9 (CEX).", command: null });
    expect(splitAnswer("```tcl\n```").command).toBeNull();
    expect(splitAnswer("```python\nprint(1)\n```").command).toBeNull();
    expect(splitAnswer("```sv\nassert property (x);\n```\nthen\n```tcl\nprove -all\n```").command).toBe("prove -all");
  });
  it("reads an unclosed fence (answer cut short)", () => {
    expect(splitAnswer("Run\n```tcl\nprove -all").command).toBe("prove -all");
  });
});

describe("prefill", () => {
  it("keeps one line, joining several with '; ' and dropping comments", () => {
    expect(prefill("prove -property a_never9")).toBe("prove -property a_never9");
    expect(prefill("# first the limit\nset_prove_time_limit 1m\n\nprove -all\n")).toBe("set_prove_time_limit 1m; prove -all");
  });
  it("joins the lines of a braced expression with a space", () => {
    expect(prefill("assert -name a {\n  req |->\n  ##1 gnt\n}\nprove -all")).toBe("assert -name a { req |-> ##1 gnt }; prove -all");
  });
});

describe("askLines", () => {
  it("prefixes every line with 'ask: ' and drops fence lines", () => {
    expect(askLines("Proving it:\n```tcl\nprove -property a_never9\n```\n")).toEqual([
      "ask: Proving it:",
      "ask: prove -property a_never9",
    ]);
    expect(askLines("")).toEqual(["ask: "]);
  });
});

describe("status bar label and tooltip", () => {
  it("names provider/model, or offline with the reason in the tooltip", () => {
    expect(llmLabel({ provider: "ollama-gemma", model: "gemma3:12b", ok: true })).toBe("LLM: ollama-gemma/gemma3:12b");
    const off = { provider: "ollama-gemma", model: "gemma3:12b", ok: false, error: "Connection refused" };
    expect(llmLabel(off)).toBe("LLM: offline");
    expect(llmTip(off)).toContain("Connection refused");
    expect(llmLabel(null)).toBe("LLM: …");
  });
});

describe("tclWord", () => {
  it("braces a name Tcl would otherwise evaluate", () => {
    expect(tclWord("a_never9")).toBe("a_never9");
    expect(tclWord("u_fifo.a_full")).toBe("u_fifo.a_full");
    expect(tclWord("gen[0].p")).toBe("{gen[0].p}");
  });
});
