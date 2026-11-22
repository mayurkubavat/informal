import type { SessionView } from "../../backend";
import { Glyph } from "../Glyph";
import { normalizeStatus } from "./index";

export function SecTab({
  session,
  idle,
  onRunTcl,
}: {
  session: SessionView | null;
  idle: boolean;
  onRunTcl: (cmd: string) => void;
}) {
  const secChecks = session?.sec?.checks ?? [];

  return (
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
        <p className="empty small">
          Configure via `check_sec -setup -spec a.sv -imp b.sv -prove` in the Tcl console.
        </p>
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
  );
}
