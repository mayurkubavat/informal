import { useEffect, useRef, useState } from "react";
import type { Flavor } from "../backend";
import { askLines, type InputMode } from "../ask";
import { completionWord, cycleCompletion, startCompletion, type Completion, type LogLine } from "../model";

// The console: scrollback, prompt, Up/Down history, and Tab completion
// of command and property names (`complete` asks the backend). Several
// matches insert the first and list them all under the prompt; repeated
// Tab / Shift+Tab cycles; typing or Esc closes the list.
//
// In Ask mode the prompt reads `ask>` and Enter sends the text to the LLM
// (`onAsk`); the answer streams into the scrollback as "ask:" lines. A
// command the answer suggests comes back as `suggestion`: the prompt turns
// into a Tcl prompt holding it, with the hint "Enter runs · Esc discards".
// Nothing runs until the user presses Enter, which submits it as a normal
// Tcl command; Esc drops it. Esc while an answer streams cancels it.
export function Console({
  hidden, log, busy, flavor, onSubmit, complete,
  mode = "tcl", asking = false, suggestion = null, onAsk, onCancelAsk, onSuggestion,
}: {
  hidden: boolean;
  log: LogLine[];
  busy: boolean;
  flavor: Flavor;
  onSubmit: (c: string) => void;
  complete?: (prefix: string) => Promise<string[]>;
  mode?: InputMode;
  // An answer is streaming.
  asking?: boolean;
  // A command the last answer suggested, waiting for Enter or Esc.
  suggestion?: string | null;
  onAsk?: (prompt: string) => void;
  onCancelAsk?: () => void;
  onSuggestion?: (how: "run" | "discard") => void;
}) {
  const [cmd, setCmd] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [hi, setHi] = useState(-1);
  const [comp, setComp] = useState<Completion | null>(null);
  // Bumped on every edit, so a slow completion reply for older text is dropped.
  const edit = useRef(0);
  const end = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const tclPrompt = `informal[${flavor}]>`;
  const pending = suggestion !== null;
  const askPrompt = mode === "ask" && !pending;
  const prompt = askPrompt ? "ask>" : tclPrompt;
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log, hidden]);
  // A suggestion arrives: put it on the prompt, caret at the end.
  useEffect(() => {
    if (suggestion === null) return;
    edit.current++;
    setCmd(suggestion);
    setComp(null);
    input.current?.focus();
  }, [suggestion]);

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

  const enter = () => {
    if (askPrompt) {
      if (asking) return;
      onAsk?.(cmd);
    } else {
      onSubmit(cmd);
      if (pending) onSuggestion?.("run");
    }
    setHistory((h) => [cmd, ...h]);
    setHi(-1);
    set("");
  };
  const escape = (e: React.KeyboardEvent) => {
    if (comp) setComp(null);
    else if (pending) {
      set("");
      onSuggestion?.("discard");
    } else if (asking) onCancelAsk?.();
    else return;
    // Used here: the workbench must not also stop a prove.
    e.preventDefault();
  };

  return (
    <div className="console" hidden={hidden}>
      <div className="console-log mono">
        {log.map((l, i) =>
          l.kind === "ask" ? (
            <div key={i} className="line ask">
              {askLines(l.text).join("\n")}
            </div>
          ) : (
            <div key={i} className={`line ${l.kind}`}>
              {l.kind === "in" ? `${tclPrompt} ${l.text}` : l.kind === "ask-in" ? `ask> ${l.text}` : l.text}
            </div>
          ),
        )}
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
      <label className={`prompt-line mono ${pending ? "suggested" : ""}`}>
        <span className="prompt">{prompt}</span>
        <input
          id="console-input"
          ref={input}
          className="mono"
          value={cmd}
          disabled={busy && !askPrompt}
          readOnly={askPrompt && asking}
          aria-label={askPrompt ? "Ask a question" : pending ? "Suggested Tcl command" : "Tcl command"}
          placeholder={
            askPrompt
              ? asking
                ? "answering… (Esc cancels)"
                : "prove only the a_never9 property"
              : "assert -name a1 {req |-> ##1 ack}"
          }
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => set(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Tab") {
              if (!askPrompt) tab(e);
            } else if (e.key === "Escape") escape(e);
            else if (e.key === "Enter" && cmd.trim()) enter();
            else if (e.key === "ArrowUp" && history.length) {
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
        {pending && <span className="prompt-hint">Enter runs · Esc discards</span>}
      </label>
    </div>
  );
}
