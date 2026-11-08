import { useEffect, useRef } from "react";
import type { EngineLine } from "../backend";

// The Log tab: raw engine output ("engine-log" events) in a monospace
// scrollback, each line tagged with its property and engine. stderr lines
// are dimmed, not coloured. Clear empties it; with autoscroll on, new lines
// keep the end in view.
export function EngineLog({
  hidden, lines, dropped, autoscroll, onAutoscroll, onClear,
}: {
  hidden: boolean;
  lines: EngineLine[];
  dropped: number;
  autoscroll: boolean;
  onAutoscroll: (on: boolean) => void;
  onClear: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (autoscroll && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [lines, autoscroll, hidden]);
  return (
    <div className="engine-log" hidden={hidden}>
      <div className="log-bar">
        <span className="muted small">
          {lines.length} line{lines.length === 1 ? "" : "s"}
          {dropped ? ` (${dropped} older dropped)` : ""}
        </span>
        <span className="spacer" />
        <label className="small check">
          <input type="checkbox" checked={autoscroll} onChange={(e) => onAutoscroll(e.target.checked)} /> Autoscroll
        </label>
        <button className="btn" onClick={onClear} disabled={!lines.length && !dropped}>
          Clear
        </button>
      </div>
      <div className="log-lines mono" ref={box} role="log" aria-label="Engine output">
        {lines.length ? (
          lines.map((l, i) => (
            <div key={i} className={`line ${l.err ? "stderr" : "stdout"}`}>
              <span className="log-tag">
                {l.prop} {l.engine}
              </span>
              {l.text}
            </div>
          ))
        ) : (
          <p className="empty">No engine output yet. External engines' stdout and stderr appear here while a prove runs.</p>
        )}
      </div>
    </div>
  );
}
