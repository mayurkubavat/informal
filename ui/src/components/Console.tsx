import { useEffect, useRef, useState } from "react";
import type { Flavor } from "../backend";
import type { LogLine } from "../model";

export function Console({ hidden, log, busy, flavor, onSubmit }: { hidden: boolean; log: LogLine[]; busy: boolean; flavor: Flavor; onSubmit: (c: string) => void }) {
  const [cmd, setCmd] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [hi, setHi] = useState(-1);
  const end = useRef<HTMLDivElement>(null);
  const prompt = `fdv[${flavor}]>`;
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log, hidden]);
  return (
    <div className="console" hidden={hidden}>
      <div className="console-log mono">
        {log.map((l, i) => (
          <div key={i} className={`line ${l.kind}`}>
            {l.kind === "in" ? `${prompt} ${l.text}` : l.text}
          </div>
        ))}
        <div ref={end} />
      </div>
      <label className="prompt-line mono">
        <span className="prompt">{prompt}</span>
        <input
          className="mono"
          value={cmd}
          disabled={busy}
          aria-label="Tcl command"
          placeholder="assert -name a1 {req |-> ##1 ack}"
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && cmd.trim()) {
              onSubmit(cmd);
              setHistory((h) => [cmd, ...h]);
              setHi(-1);
              setCmd("");
            } else if (e.key === "ArrowUp" && history.length) {
              e.preventDefault();
              const n = Math.min(hi + 1, history.length - 1);
              setHi(n);
              setCmd(history[n]);
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              const n = Math.max(hi - 1, -1);
              setHi(n);
              setCmd(n >= 0 ? history[n] : "");
            }
          }}
        />
      </label>
    </div>
  );
}
