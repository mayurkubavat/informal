import type { SessionView } from "../../backend";
import { Glyph } from "../Glyph";
import { normalizeStatus } from "./index";

export function XpropTab({
  session,
  idle,
  onRunTcl,
}: {
  session: SessionView | null;
  idle: boolean;
  onRunTcl: (cmd: string) => void;
}) {
  const xpropItems = session?.xprop?.items ?? [];

  return (
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
        <p className="empty small">
          Click Prove X-Prop (or run `check_xprop -prove`) to audit reset-less flops and X sources.
        </p>
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
  );
}
