import type { EngineRun, RunOutcome } from "../backend";
import { barShare, msText, profileSummary, type ViewProp } from "../model";

const OUTCOME: Record<RunOutcome, string> = { won: "Won", lost: "Lost", timeout: "Timeout", killed: "Killed", error: "Error" };

// A plain horizontal time bar: grey, the winner's in the text colour.
function Bar({ share, win, label }: { share: number; win: boolean; label: string }) {
  return (
    <span className="prof-track" role="img" aria-label={label}>
      <span className={`prof-bar ${win ? "win" : ""}`} style={{ width: `${(share * 100).toFixed(1)}%` }} />
    </span>
  );
}

// The Profiler dock tab: for the selected property, every portfolio engine's
// result, time and depth reached; for the whole run, each engine's wins,
// total time and mean time-to-win. No colour beyond the frame's own.
export function Profiler({ prop, props }: { prop: ViewProp | null; props: ViewProp[] }) {
  const runs: EngineRun[] = prop?.engines ?? [];
  const maxRun = Math.max(0, ...runs.map((r) => r.ms));
  const summary = profileSummary(props);
  const maxTotal = Math.max(0, ...summary.map((s) => s.totalMs));
  const proved = props.filter((p) => p.engines?.length).length;
  return (
    <div className="profiler">
      <section className="prof-pane" aria-label="Engines on the selected property">
        <div className="prof-head">
          {prop ? (
            <>
              Engines on <span className="mono strong">{prop.name}</span>
            </>
          ) : (
            "Engines on the selected property"
          )}
        </div>
        {runs.length ? (
          <table className="grid prof-table">
            <thead>
              <tr>
                <th>Engine</th>
                <th>Result</th>
                <th className="num">Time</th>
                <th className="num">Depth</th>
                <th className="prof-barcol">Time bar</th>
              </tr>
            </thead>
            <tbody>
              {runs.map((r) => (
                <tr key={r.engine} className={r.outcome === "won" ? "prof-won" : ""}>
                  <td className="mono">{r.engine}</td>
                  <td>{OUTCOME[r.outcome]}</td>
                  <td className="num mono">{msText(r.ms)}</td>
                  <td className="num mono">{r.depth ?? "—"}</td>
                  <td className="prof-barcol">
                    <Bar share={barShare(r.ms, maxRun)} win={r.outcome === "won"} label={`${r.engine} ${msText(r.ms)}`} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <p className="empty">{prop ? "No engine runs recorded for this property yet. Prove it to profile the portfolio." : "Select a property to see how each engine did on it."}</p>
        )}
      </section>
      <section className="prof-pane" aria-label="Engines over the run">
        <div className="prof-head">
          Run summary <span className="muted">· {proved} propert{proved === 1 ? "y" : "ies"} profiled</span>
        </div>
        {summary.length ? (
          <table className="grid prof-table">
            <thead>
              <tr>
                <th>Engine</th>
                <th className="num">Wins</th>
                <th className="num">Runs</th>
                <th className="num">Total time</th>
                <th className="num">Mean to win</th>
                <th className="prof-barcol">Total time bar</th>
              </tr>
            </thead>
            <tbody>
              {summary.map((s, i) => (
                <tr key={s.engine}>
                  <td className="mono">{s.engine}</td>
                  <td className="num mono">{s.wins}</td>
                  <td className="num mono">{s.runs}</td>
                  <td className="num mono">{msText(s.totalMs)}</td>
                  <td className="num mono">{s.meanWinMs == null ? "—" : msText(s.meanWinMs)}</td>
                  <td className="prof-barcol">
                    <Bar share={barShare(s.totalMs, maxTotal)} win={i === 0 && s.wins > 0} label={`${s.engine} total ${msText(s.totalMs)}`} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <p className="empty">No profile yet. Run a prove; each property's portfolio engines are recorded here.</p>
        )}
      </section>
    </div>
  );
}
