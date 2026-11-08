import { useEffect, useRef, useState } from "react";
import type { Flavor } from "../backend";
import { completionWord, cycleCompletion, startCompletion, type Completion, type LogLine } from "../model";

// The Tcl console: scrollback, prompt, Up/Down history, and Tab completion
// of command and property names (`complete` asks the backend). Several
// matches insert the first and list them all under the prompt; repeated
// Tab / Shift+Tab cycles; typing or Esc closes the list.
export function Console({
  hidden, log, busy, flavor, onSubmit, complete,
}: {
  hidden: boolean;
  log: LogLine[];
  busy: boolean;
  flavor: Flavor;
  onSubmit: (c: string) => void;
  complete?: (prefix: string) => Promise<string[]>;
}) {
  const [cmd, setCmd] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [hi, setHi] = useState(-1);
  const [comp, setComp] = useState<Completion | null>(null);
  // Bumped on every edit, so a slow completion reply for older text is dropped.
  const edit = useRef(0);
  const end = useRef<HTMLDivElement>(null);
  const prompt = `fdv[${flavor}]>`;
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log, hidden]);

  const set = (v: string) => {
    edit.current++;
    setCmd(v);
    setComp(null);
  };
  const tab = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (comp && cmd === comp.base + comp.matches[comp.index]) {
      e.preventDefault();
      const next = cycleCompletion(comp, e.shiftKey ? -1 : 1);
      setCmd(next.cmd);
      setComp(next.state);
      return;
    }
    const { word } = completionWord(cmd);
    // Nothing to complete: let Tab move focus as usual.
    if (!word || !complete || e.shiftKey) return;
    e.preventDefault();
    const asked = cmd;
    const at = edit.current;
    complete(word).then(
      (matches) => {
        if (edit.current !== at) return;
        const r = startCompletion(asked, matches);
        setCmd(r.cmd);
        setComp(r.state);
      },
      () => {},
    );
  };

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
      {comp && (
        <div className="completions mono" role="listbox" aria-label="Completions">
          {comp.matches.map((m, i) => (
            <span key={m} role="option" aria-selected={i === comp.index} className={i === comp.index ? "current" : undefined}>
              {m}
            </span>
          ))}
        </div>
      )}
      <label className="prompt-line mono">
        <span className="prompt">{prompt}</span>
        <input
          id="console-input"
          className="mono"
          value={cmd}
          disabled={busy}
          aria-label="Tcl command"
          placeholder="assert -name a1 {req |-> ##1 ack}"
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => set(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Tab") tab(e);
            else if (e.key === "Escape" && comp) {
              e.preventDefault();
              setComp(null);
            } else if (e.key === "Enter" && cmd.trim()) {
              onSubmit(cmd);
              setHistory((h) => [cmd, ...h]);
              setHi(-1);
              set("");
            } else if (e.key === "ArrowUp" && history.length) {
              e.preventDefault();
              const n = Math.min(hi + 1, history.length - 1);
              setHi(n);
              set(history[n]);
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              const n = Math.max(hi - 1, -1);
              setHi(n);
              set(n >= 0 ? history[n] : "");
            }
          }}
        />
      </label>
    </div>
  );
}
