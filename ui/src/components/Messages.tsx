import type { LogEvent } from "../model";
import { Glyph } from "./Glyph";

export function Messages({ events }: { events: LogEvent[] }) {
  if (!events.length) return <p className="empty">No messages.</p>;
  return (
    <div className="messages">
      <table className="grid">
        <tbody>
          {[...events].reverse().map((e, i) => (
            <tr key={i} className="static">
              <td className="mono dim">{e.at.toLocaleTimeString()}</td>
              <td>
                <span className="status">
                  <Glyph k={e.ok ? "proven" : "error"} />
                  {e.ok ? "Info" : "Error"}
                </span>
              </td>
              <td className="mono">{e.cmd}</td>
              <td className="dim msg-text">{e.summary}</td>
              <td className="mono num dim">{Math.round(e.ms)} ms</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
