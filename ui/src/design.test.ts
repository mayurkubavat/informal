import { describe, expect, it } from "vitest";
import { designScopes, findScope, parsePorts, signalKind } from "./model";
import { MOCK_FIFO, mockSource } from "./mock";

describe("design browser helpers", () => {
  it("uses slang's hierarchy when there is one", () => {
    expect(designScopes(MOCK_FIFO, "fifo", [])).toBe(MOCK_FIFO);
  });
  it("falls back to the top module with its scanned ports, widths evaluated when plain", () => {
    const s = designScopes([], "counter", parsePorts(mockSource));
    expect(s).toHaveLength(1);
    expect(s[0].signals.map((x) => [x.name, x.width, x.dir])).toEqual([
      ["clk", 1, "input"], ["rst_n", 1, "input"], ["en", 1, "input"], ["q", 4, "output"], ["done", 1, "output"],
    ]);
    expect(designScopes(undefined, "m", [{ dir: "input", name: "d", width: "[W-1:0]" }])[0].signals[0].width).toBe(0);
    expect(designScopes(undefined, null, [])).toEqual([]);
  });
  it("finds nested scopes and names kinds briefly", () => {
    expect(findScope(MOCK_FIFO, "fifo.u_rp")?.module).toBe("ptr");
    expect(findScope(MOCK_FIFO, "nope")).toBeNull();
    expect(signalKind({ kind: "port", dir: "output" })).toBe("out");
    expect(signalKind({ kind: "variable" })).toBe("var");
    expect(signalKind({ kind: "parameter" })).toBe("param");
  });
});
