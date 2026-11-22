import type { SessionView } from "../../backend";
import { Glyph } from "../Glyph";
import { normalizeStatus } from "./index";

export function SpvTab({
  session,
  idle,
  spvFrom,
  setSpvFrom,
  spvTo,
  setSpvTo,
  onRunTcl,
}: {
  session: SessionView | null;
  idle: boolean;
  spvFrom: string;
  setSpvFrom: (v: string) => void;
  spvTo: string;
  setSpvTo: (v: string) => void;
  onRunTcl: (cmd: string) => void;
}) {
  const spvRules = session?.spv ?? [];

  return (
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
        <p className="empty small">
          Add a 2-safety taint non-interference rule (`-from` secret to `-to` observable) and prove.
        </p>
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
  );
}
