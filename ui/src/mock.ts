// Browser mock for `npm run dev` and the Playwright suites: one counter
// design with six properties. `prove`/`check_fv` resolve them to fixed
// results; any other Tcl answers with an error line.

import type { PropStatus, Reply, SessionView } from "./backend";

export const mockSource = `module counter (input logic clk, input logic rst_n, input logic en,
                output logic [3:0] q, output logic done);
    assign done = (q == 4'd9);
    always_ff @(posedge clk or negedge rst_n)
        if (!rst_n) q <= 4'd0;
        else if (en && !done) q <= q + 4'd1;
endmodule
`;

const mockSession: SessionView = {
  design: { files: ["tests/fpv/counter/counter.sv"], top: "counter", defines: [] },
  elaborated: true,
  clocks: [{ signal: "clk", edge: "posedge" }],
  reset: "!rst_n",
  props: [
    { name: "a_max", kind: "assert", expr: "q <= 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "a_step", kind: "assert", expr: "en && !done |=> q == $past(q) + 4'd1", enabled: true, status: { status: "unprocessed" } },
    { name: "a_never9", kind: "assert", expr: "q != 4'd9", enabled: true, status: { status: "unprocessed" } },
    { name: "c_done", kind: "cover", expr: "$rose(done)", enabled: true, status: { status: "unprocessed" } },
    { name: "c_10", kind: "cover", expr: "q == 4'd10", enabled: true, status: { status: "unprocessed" } },
    { name: "m_en", kind: "assume", expr: "en |=> en", enabled: true, status: { status: "unprocessed" } },
  ],
  time_limit: { secs: 60, nanos: 0 },
  workdir: "fdv_work",
};

const mockResults: Record<string, PropStatus> = {
  a_max: { status: "proven", engine: "ric3-ic3", ms: 41 },
  a_step: { status: "proven", engine: "ric3", ms: 55 },
  a_never9: { status: "cex", engine: "ric3-bmc", depth: 11, ms: 30 },
  c_done: { status: "covered", engine: "ric3-bmc", depth: 11, ms: 31 },
  c_10: { status: "unreachable", engine: "ric3-ic3", ms: 29 },
};

export async function mock(cmd: string, args: Record<string, unknown>): Promise<Reply> {
  await new Promise((r) => setTimeout(r, 150));
  const script = String(args.script ?? "");
  if (cmd === "shell_eval" && /^\s*(prove|check_fv)\b/.test(script)) {
    for (const p of mockSession.props) {
      if (p.kind !== "assume" && p.enabled) p.status = mockResults[p.name] ?? { status: "undetermined", reason: "mock" };
    }
    return { ok: true, result: "proven 2 cex 1 covered 1 unreachable 1 undetermined 0 error 0", session: mockSession };
  }
  if (cmd === "shell_eval" && script.trim() !== "") {
    return { ok: false, result: "browser preview: Tcl runs only in the desktop app", session: mockSession };
  }
  return { ok: true, result: "", session: mockSession };
}
