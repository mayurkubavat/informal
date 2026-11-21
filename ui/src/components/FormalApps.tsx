import { useState } from "react";
import type { SessionView } from "../backend";
import { Glyph } from "./Glyph";

export type AppMode = "cov" | "xprop" | "spv" | "sec" | "csr" | "protocol";

function normalizeStatus(st: string | { cex?: { depth?: number | null } } | undefined): string {
  if (!st) return "unprocessed";
  if (typeof st === "string") return st;
  if ("cex" in st) return "cex";
  return "unprocessed";
}

export function FormalApps({
  session,
  mode,
  onMode,
  idle,
  onRunTcl,
  onSelectTask,
}: {
  session: SessionView | null;
  mode: AppMode;
  onMode: (m: AppMode) => void;
  idle: boolean;
  onRunTcl: (cmd: string) => void;
  onSelectTask: (task: string) => void;
}) {
  const [spvFrom, setSpvFrom] = useState("");
  const [spvTo, setSpvTo] = useState("");
  const [protoKind, setProtoKind] = useState<"tlul" | "axi4lite" | "apb">("apb");
  const [protoRole, setProtoRole] = useState<"slave" | "master" | "monitor">("slave");

  const covItems = session?.cov?.items ?? [];
  const covCovered = covItems.filter((i) => i.status === "covered").length;
  const covUnr = covItems.filter((i) => i.status === "unreachable").length;

  const xpropItems = session?.xprop?.items ?? [];
  const spvRules = session?.spv ?? [];
  const secChecks = session?.sec?.checks ?? [];
  const connProps = (session?.props ?? []).filter((p) => p.task === "Connectivity");
  const csrProps = (session?.props ?? []).filter((p) => p.task === "CSR");
  const protoProps = (session?.props ?? []).filter((p) => p.task === "Protocol");

  return (
    <div className="formal-apps" aria-label="Formal verification apps">
      <div className="apps-nav" role="tablist" aria-label="Formal app">
        {(
          [
            ["cov", `Coverage (${covItems.length})`],
            ["xprop", `X-Prop (${xpropItems.length})`],
            ["spv", `SPV (${spvRules.length})`],
            ["sec", `SEC / LEC (${secChecks.length})`],
            ["csr", `CSR / Conn (${csrProps.length + connProps.length})`],
            ["protocol", `Bus VIP (${protoProps.length})`],
          ] as [AppMode, string][]
        ).map(([k, label]) => (
          <button
            key={k}
            role="tab"
            aria-selected={mode === k}
            className={`apps-tab ${mode === k ? "on" : ""}`}
            onClick={() => onMode(k)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="apps-body">
        {mode === "cov" && (
          <div>
            <div className="apps-bar">
              <span>
                COI & UNR: <strong>{covCovered}</strong> covered · <strong>{covUnr}</strong> UNR unreachable
              </span>
              <span className="spacer" />
              <button
                className="btn small"
                disabled={!idle || !session?.elaborated}
                onClick={() => onRunTcl("check_cov -init -model {coi branch toggle}; check_cov -measure")}
              >
                Measure COI & UNR
              </button>
            </div>
            {covItems.length === 0 ? (
              <p className="empty small">Run Measure COI & UNR (or `check_cov -measure`) to populate COI and dead-code targets.</p>
            ) : (
              <table className="apps-table">
                <thead>
                  <tr>
                    <th>Model</th>
                    <th>Target</th>
                    <th>Status</th>
                    <th>Location</th>
                  </tr>
                </thead>
                <tbody>
                  {covItems.slice(0, 100).map((it) => (
                    <tr key={it.id}>
                      <td className="mono">{it.model}</td>
                      <td className="mono">{it.name}</td>
                      <td>
                        <span className="status">
                          <Glyph k={it.status} />
                          {it.status}
                        </span>
                      </td>
                      <td className="mono muted">{it.file && it.line ? `${it.file.split("/").pop()}:${it.line}` : "—"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}

        {mode === "xprop" && (
          <div>
            <div className="apps-bar">
              <span>Exact 2-copy self-composition X-propagation</span>
              <span className="spacer" />
              <button
                className="btn small"
                disabled={!idle || !session?.elaborated}
                onClick={() => onRunTcl("check_xprop -init; check_xprop -prove")}
              >
                Prove X-Prop
              </button>
            </div>
            {xpropItems.length === 0 ? (
              <p className="empty small">Click Prove X-Prop (or run `check_xprop -prove`) to audit reset-less flops and X sources.</p>
            ) : (
              <table className="apps-table">
                <thead>
                  <tr>
                    <th>Sink</th>
                    <th>Category</th>
                    <th>Status</th>
                    <th>Engine</th>
                  </tr>
                </thead>
                <tbody>
                  {xpropItems.map((it) => {
                    const st = normalizeStatus(it.status);
                    return (
                      <tr key={it.name}>
                        <td className="mono">{it.name}</td>
                        <td>{it.category}</td>
                        <td>
                          <span className="status">
                            <Glyph k={st} />
                            {st}
                          </span>
                        </td>
                        <td className="mono muted">{it.engine ?? "—"}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </div>
        )}

        {mode === "spv" && (
          <div>
            <div className="apps-bar">
              <input
                className="filter mono"
                placeholder="-from secret_sig"
                aria-label="SPV secret source"
                value={spvFrom}
                onChange={(e) => setSpvFrom(e.target.value)}
              />
              <input
                className="filter mono"
                placeholder="-to obs_sig"
                aria-label="SPV observable sink"
                value={spvTo}
                onChange={(e) => setSpvTo(e.target.value)}
              />
              <button
                className="btn small"
                disabled={!idle || !spvFrom.trim() || !spvTo.trim()}
                onClick={() => {
                  onRunTcl(`check_spv -create -from {${spvFrom.trim()}} -to {${spvTo.trim()}}`);
                  setSpvFrom("");
                  setSpvTo("");
                }}
              >
                Add SPV Rule
              </button>
              <span className="spacer" />
              <button
                className="btn small"
                disabled={!idle || spvRules.length === 0}
                onClick={() => onRunTcl("check_spv -prove")}
              >
                Prove SPV
              </button>
            </div>
            {spvRules.length === 0 ? (
              <p className="empty small">Add a 2-safety taint non-interference rule (`-from` secret to `-to` observable) and prove.</p>
            ) : (
              <table className="apps-table">
                <thead>
                  <tr>
                    <th>Rule</th>
                    <th>From</th>
                    <th>To</th>
                    <th>Status</th>
                  </tr>
                </thead>
                <tbody>
                  {spvRules.map((r) => {
                    const st = normalizeStatus(r.status);
                    return (
                      <tr key={r.name}>
                        <td className="mono">{r.name}</td>
                        <td className="mono">{r.from.join(", ")}</td>
                        <td className="mono">{r.to.join(", ")}</td>
                        <td>
                          <span className="status">
                            <Glyph k={st} />
                            {st}
                          </span>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </div>
        )}

        {mode === "sec" && (
          <div>
            <div className="apps-bar">
              <span>
                {session?.sec?.setup
                  ? `${session.sec.setup.lec ? "LEC" : "SEC"}: ${session.sec.setup.spec_top ?? "spec"} vs ${session.sec.setup.imp_top ?? "imp"} (latency +${session.sec.setup.latency})`
                  : "Dual-design Sequential & Combinational Equivalence (`check_sec`)"}
              </span>
              <span className="spacer" />
              <button
                className="btn small"
                disabled={!idle || !session?.sec?.setup}
                onClick={() => onRunTcl("check_sec -prove")}
              >
                Prove SEC / LEC
              </button>
            </div>
            {secChecks.length === 0 ? (
              <p className="empty small">Configure via `check_sec -setup -spec a.sv -imp b.sv -prove` in the Tcl console.</p>
            ) : (
              <table className="apps-table">
                <thead>
                  <tr>
                    <th>Check</th>
                    <th>Kind</th>
                    <th>Pair</th>
                    <th>Status</th>
                  </tr>
                </thead>
                <tbody>
                  {secChecks.map((c) => {
                    const st = normalizeStatus(c.status);
                    return (
                      <tr key={c.name}>
                        <td className="mono">{c.name}</td>
                        <td>{c.kind}</td>
                        <td className="mono">{`${c.spec_sig} ↔ ${c.imp_sig} [${c.width}b]`}</td>
                        <td>
                          <span className="status">
                            <Glyph k={st} />
                            {st}
                          </span>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </div>
        )}

        {mode === "csr" && (
          <div>
            <div className="apps-bar">
              <span>
                CSR assertions: <strong>{csrProps.length}</strong> · Connectivity assertions: <strong>{connProps.length}</strong>
              </span>
              <span className="spacer" />
              {csrProps.length > 0 && (
                <button className="btn small" onClick={() => onSelectTask("CSR")}>
                  Show CSR Task
                </button>
              )}
              {connProps.length > 0 && (
                <button className="btn small" onClick={() => onSelectTask("Connectivity")}>
                  Show Connectivity Task
                </button>
              )}
            </div>
            <p className="empty small">
              Load specs via `check_csr -load regs.hjson -prove` (HJSON, CSV, or UVM `uvm_reg_block` `.sv`) or `check_conn -load map.csv -prove`.
            </p>
          </div>
        )}

        {mode === "protocol" && (
          <div>
            <div className="apps-bar">
              <select aria-label="Bus protocol" value={protoKind} onChange={(e) => setProtoKind(e.target.value as "tlul" | "axi4lite" | "apb")}>
                <option value="apb">AMBA APB</option>
                <option value="axi4lite">AMBA AXI4-Lite</option>
                <option value="tlul">TileLink-UL</option>
              </select>
              <select aria-label="Bus role" value={protoRole} onChange={(e) => setProtoRole(e.target.value as "slave" | "master" | "monitor")}>
                <option value="slave">Slave DUT</option>
                <option value="master">Master DUT</option>
                <option value="monitor">Monitor</option>
              </select>
              <button
                className="btn small"
                disabled={!idle || !session?.elaborated}
                onClick={() => onRunTcl(`check_protocol -create -protocol ${protoKind} -role ${protoRole}`)}
              >
                Generate VIP
              </button>
              <span className="spacer" />
              {protoProps.length > 0 && (
                <button className="btn small" onClick={() => onSelectTask("Protocol")}>
                  Show Protocol Task ({protoProps.length})
                </button>
              )}
            </div>
            <p className="empty small">
              Generates formal handshake, payload-stability, opcode-legality, and transaction cover properties in task `Protocol`.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
