// UI v1: property manager, Tcl console and source view over one fdv shell.
// Waveforms (Surfer) join when the engines export counterexample traces.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, Flavor, Property, Reply, SessionView, StatusWord } from "./backend";

type LogLine = { kind: "in" | "out" | "err"; text: string };

const STATUS_LABEL: Record<StatusWord, string> = {
  unprocessed: "Unprocessed",
  proven: "Proven",
  cex: "CEX",
  covered: "Covered",
  unreachable: "Unreachable",
  undetermined: "Undetermined",
  error: "Error",
};

function detail(p: Property): string {
  const s = p.status;
  const secs = s.ms !== undefined ? `${(s.ms / 1000).toFixed(2)} s` : "";
  switch (s.status) {
    case "cex":
    case "covered":
      return `depth ${s.depth ?? "?"} · ${secs}`;
    case "proven":
    case "unreachable":
      return secs;
    case "undetermined":
      return s.reason ?? "";
    case "error":
      return (s.message ?? "").split("\n")[0];
    default:
      return "";
  }
}

export default function App() {
  const [session, setSession] = useState<SessionView | null>(null);
  const [flavor, setFlavor] = useState<Flavor>("jg");
  const [scriptPath, setScriptPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [log, setLog] = useState<LogLine[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [source, setSource] = useState("");

  const apply = useCallback((r: Reply, echo?: string) => {
    if (r.session) setSession(r.session);
    setLog((l) => [
      ...l,
      ...(echo ? [{ kind: "in" as const, text: echo }] : []),
      ...(r.result ? [{ kind: r.ok ? ("out" as const) : ("err" as const), text: r.result }] : []),
    ]);
  }, []);

  const run = useCallback(
    async (f: () => Promise<Reply>, echo?: string) => {
      setBusy(true);
      try {
        apply(await f(), echo);
      } catch (e) {
        setLog((l) => [...l, { kind: "err", text: String(e) }]);
      } finally {
        setBusy(false);
      }
    },
    [apply],
  );

  useEffect(() => {
    run(() => backend.snapshot());
  }, [run]);

  const topFile = session?.design.files[0];
  useEffect(() => {
    if (topFile) backend.readText(topFile).then(setSource, () => setSource(""));
  }, [topFile]);

  const props = useMemo(
    () => (session?.props ?? []).filter((p) => p.name.includes(filter) || p.expr.includes(filter)),
    [session, filter],
  );
  const counts = useMemo(() => {
    const c: Partial<Record<StatusWord, number>> = {};
    for (const p of session?.props ?? []) if (p.kind !== "assume") c[p.status.status] = (c[p.status.status] ?? 0) + 1;
    return c;
  }, [session]);
  const current = session?.props.find((p) => p.name === selected) ?? null;
  const proveCmd = flavor === "jg" ? "prove -all" : "check_fv";

  return (
    <div className="app">
      <header className="toolbar">
        <span className="brand">Formal DV</span>
        <label className="field">
          Flavor
          <select
            value={flavor}
            disabled={busy}
            onChange={(e) => {
              const f = e.target.value as Flavor;
              setFlavor(f);
              run(() => backend.setFlavor(f));
            }}
          >
            <option value="jg">JasperGold</option>
            <option value="vcf">VC Formal</option>
          </select>
        </label>
        <input
          className="path"
          placeholder="Script path, e.g. tests/fpv/counter/counter.jg.tcl"
          value={scriptPath}
          onChange={(e) => setScriptPath(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && scriptPath && run(() => backend.source(scriptPath), `source ${scriptPath}`)}
        />
        <button disabled={busy || !scriptPath} onClick={() => run(() => backend.source(scriptPath), `source ${scriptPath}`)}>
          Run script
        </button>
        <button className="primary" disabled={busy || !session?.props.length} onClick={() => run(() => backend.evalTcl(proveCmd), proveCmd)}>
          {busy ? "Running…" : "Prove all"}
        </button>
        {!backend.inTauri && <span className="badge mock">Browser preview</span>}
      </header>

      <section className="summary">
        <span>Top <b>{session?.design.top ?? "—"}</b></span>
        <span>Clock <b>{session?.clocks.map((c) => c.signal).join(", ") || "—"}</b></span>
        <span>Reset <b>{session?.reset ?? "—"}</b></span>
        <span className="spacer" />
        {(["proven", "cex", "covered", "unreachable", "undetermined", "error"] as StatusWord[]).map((s) => (
          <span key={s} className={`count st-${s}`}>
            {STATUS_LABEL[s]} <b>{counts[s] ?? 0}</b>
          </span>
        ))}
      </section>

      <main className="panes">
        <section className="props">
          <div className="pane-head">
            <h2>Properties</h2>
            <input placeholder="Filter" value={filter} onChange={(e) => setFilter(e.target.value)} />
          </div>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Type</th>
                  <th>Status</th>
                  <th>Engine</th>
                  <th>Result</th>
                </tr>
              </thead>
              <tbody>
                {props.map((p) => (
                  <tr
                    key={p.name}
                    className={`${p.name === selected ? "sel" : ""} ${p.enabled ? "" : "off"}`}
                    onClick={() => setSelected(p.name)}
                  >
                    <td className="mono">{p.name}</td>
                    <td>{p.kind}</td>
                    <td>
                      <span className={`pill st-${p.kind === "assume" ? "assume" : p.status.status}`}>
                        {p.kind === "assume" ? "Assumed" : p.enabled ? STATUS_LABEL[p.status.status] : "Disabled"}
                      </span>
                    </td>
                    <td className="mono">{p.status.engine ?? ""}</td>
                    <td>{detail(p)}</td>
                  </tr>
                ))}
                {props.length === 0 && (
                  <tr>
                    <td colSpan={5} className="empty">
                      No properties yet. Run a JasperGold or VC Formal script, or type commands in the console.
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </section>

        <aside className="detail">
          <div className="pane-head">
            <h2>{current ? current.name : "Details"}</h2>
          </div>
          {current ? (
            <div className="detail-body">
              <div className="kv"><span>Type</span><b>{current.kind}</b></div>
              <div className="kv"><span>Status</span><b>{STATUS_LABEL[current.status.status]}</b></div>
              <div className="kv"><span>Result</span><b>{detail(current) || "—"}</b></div>
              <pre className="expr">{current.expr}</pre>
              {(current.status.status === "cex" || current.status.status === "covered") && (
                <p className="note">Waveforms open here once engines export counterexample traces.</p>
              )}
            </div>
          ) : (
            <p className="note">Select a property to see its expression and result.</p>
          )}
          <div className="pane-head"><h2>Source {topFile ? `· ${topFile.split("/").pop()}` : ""}</h2></div>
          <SourceView text={source} highlight={current ? identifiers(current.expr) : []} />
        </aside>
      </main>

      <Console log={log} busy={busy} prompt={`fdv[${flavor}]>`} onSubmit={(cmd) => run(() => backend.evalTcl(cmd), cmd)} />
    </div>
  );
}

function identifiers(expr: string): string[] {
  const skip = new Set(["past", "rose", "fell", "stable", "changed", "posedge", "negedge", "disable", "iff"]);
  return [...new Set(expr.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [])].filter((w) => !skip.has(w) && !/^\d/.test(w));
}

// Lines that mention a signal of the selected property are marked, a
// first step toward cross-probing from property to RTL.
function SourceView({ text, highlight }: { text: string; highlight: string[] }) {
  const re = highlight.length ? new RegExp(`\\b(${highlight.join("|")})\\b`) : null;
  return (
    <pre className="source">
      {text.split("\n").map((line, i) => (
        <div key={i} className={re && re.test(line) ? "hit" : ""}>
          <span className="ln">{i + 1}</span>
          {line}
        </div>
      ))}
    </pre>
  );
}

function Console({ log, busy, prompt, onSubmit }: { log: LogLine[]; busy: boolean; prompt: string; onSubmit: (c: string) => void }) {
  const [cmd, setCmd] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [hi, setHi] = useState(-1);
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log]);
  return (
    <section className="console">
      <div className="console-log">
        {log.map((l, i) => (
          <div key={i} className={`line ${l.kind}`}>
            {l.kind === "in" ? `${prompt} ${l.text}` : l.text}
          </div>
        ))}
        <div ref={end} />
      </div>
      <div className="console-input">
        <span className="mono">{prompt}</span>
        <input
          className="mono"
          value={cmd}
          disabled={busy}
          placeholder="Tcl command, e.g. assert -name a1 {req |-> ##1 ack}"
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && cmd.trim()) {
              onSubmit(cmd);
              setHistory((h) => [cmd, ...h]);
              setHi(-1);
              setCmd("");
            } else if (e.key === "ArrowUp" && history.length) {
              const n = Math.min(hi + 1, history.length - 1);
              setHi(n);
              setCmd(history[n]);
            } else if (e.key === "ArrowDown") {
              const n = hi - 1;
              setHi(n);
              setCmd(n >= 0 ? history[n] : "");
            }
          }}
        />
      </div>
    </section>
  );
}
