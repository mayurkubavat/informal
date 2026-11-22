import type { SessionView } from "../../backend";
import { Glyph } from "../Glyph";

export function CovTab({
  session,
  idle,
  onRunTcl,
}: {
  session: SessionView | null;
  idle: boolean;
  onRunTcl: (cmd: string) => void;
}) {
  const covItems = session?.cov?.items ?? [];
  const covCovered = covItems.filter((i) => i.status === "covered").length;
  const covUnr = covItems.filter((i) => i.status === "unreachable").length;

  return (
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
        <p className="empty small">
          Run Measure COI & UNR (or `check_cov -measure`) to populate COI and dead-code targets.
        </p>
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
                <td className="mono muted">
                  {it.file && it.line ? `${it.file.split("/").pop()}:${it.line}` : "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
