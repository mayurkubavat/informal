// UI v2: a dense, calm EDA workbench over one fdv shell. Toolbar on top,
// left dock (design hierarchy / tasks), document tabs in the centre
// (properties / source), property inspector on the right, console + log
// dock at the bottom and a status bar. Waveforms (Surfer) join the Trace
// tab once the engines export counterexample traces.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, Flavor, Property, Reply, SessionView, StatusWord } from "./backend";

type LogLine = { kind: "in" | "out" | "err"; text: string };
type Event = { at: Date; cmd: string; ok: boolean; ms: number; summary: string };
type Theme = "system" | "light" | "dark";
type Doc = "properties" | "source" | "trace";
type LeftDock = "design" | "tasks";
type SortKey = "name" | "kind" | "status" | "engine" | "result" | "time";

const STATUS_LABEL: Record<StatusWord, string> = {
  unprocessed: "Unprocessed",
  proven: "Proven",
  cex: "CEX",
  covered: "Covered",
  unreachable: "Unreachable",
  undetermined: "Undetermined",
  error: "Error",
};
const SUMMARY: StatusWord[] = ["proven", "cex", "covered", "unreachable", "undetermined", "error"];
const STATUS_ORDER: StatusWord[] = ["error", "cex", "undetermined", "covered", "unreachable", "proven", "unprocessed"];

// Result column: the outcome without the run time (Time has its own column).
function result(p: Property): string {
  const s = p.status;
  switch (s.status) {
    case "cex":
    case "covered":
      return `depth ${s.depth ?? "?"}`;
    case "proven":
      return "full proof";
    case "unreachable":
      return "unreachable";
    case "undetermined":
      return s.reason ?? "";
    case "error":
      return (s.message ?? "").split("\n")[0];
    default:
      return "";
  }
}
function time(p: Property): string {
  return p.status.ms !== undefined ? `${(p.status.ms / 1000).toFixed(2)} s` : "";
}
function statusKey(p: Property): string {
  if (p.kind === "assume") return "assume";
  return p.enabled ? p.status.status : "unprocessed";
}
function statusText(p: Property): string {
  if (p.kind === "assume") return "Assumed";
  return p.enabled ? STATUS_LABEL[p.status.status] : "Disabled";
}

function useTheme(): [Theme, () => void] {
  const [theme, setTheme] = useState<Theme>(() => {
    try {
      return (localStorage.getItem("fdv-theme") as Theme) || "system";
    } catch {
      return "system";
    }
  });
  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    try {
      localStorage.setItem("fdv-theme", theme);
    } catch {
      /* storage unavailable */
    }
  }, [theme]);
  return [theme, () => setTheme((t) => (t === "system" ? "light" : t === "light" ? "dark" : "system"))];
}

export default function App() {
  const [session, setSession] = useState<SessionView | null>(null);
  const [flavor, setFlavor] = useState<Flavor>("jg");
  const [scriptPath, setScriptPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [log, setLog] = useState<LogLine[]>([]);
  const [events, setEvents] = useState<Event[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusWord | null>(null);
  const [source, setSource] = useState("");
  const [doc, setDoc] = useState<Doc>("properties");
  const [left, setLeft] = useState<LeftDock | null>("design");
  const [bottomTab, setBottomTab] = useState<"console" | "log">("console");
  const [bottomH, setBottomH] = useState(200);
  const [theme, cycleTheme] = useTheme();

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
      const t0 = performance.now();
      const note = (ok: boolean, text: string) =>
        echo &&
        setEvents((e) => [
          ...e,
          { at: new Date(), cmd: echo, ok, ms: performance.now() - t0, summary: text.split("\n")[0] },
        ]);
      try {
        const r = await f();
        apply(r, echo);
        note(r.ok, r.result);
      } catch (e) {
        setLog((l) => [...l, { kind: "err", text: String(e) }]);
        note(false, String(e));
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

  const allProps = session?.props ?? [];
  // Recomputed every render (cheap): the browser mock mutates the session in
  // place, so memoising on the session reference would go stale.
  const props = allProps.filter(
    (p) =>
      (p.name.includes(filter) || p.expr.includes(filter)) &&
      (!statusFilter || (p.kind !== "assume" && p.status.status === statusFilter)),
  );
  const counts: Partial<Record<StatusWord, number>> = {};
  for (const p of allProps) if (p.kind !== "assume") counts[p.status.status] = (counts[p.status.status] ?? 0) + 1;
  const current = allProps.find((p) => p.name === selected) ?? null;
  const proveCmd = flavor === "jg" ? "prove -all" : "check_fv";
  const reproveCmd = (n: string) => (flavor === "jg" ? `prove -property ${n}` : `check_fv -property {${n}}`);
  const runScript = () => scriptPath && run(() => backend.source(scriptPath), `source ${scriptPath}`);
  const changeFlavor = (f: Flavor) => {
    setFlavor(f);
    run(() => backend.setFlavor(f));
  };

  const highlight = current ? identifiers(current.expr) : [];
  const focusLine = locate(source, current);
  const fileName = topFile?.split("/").pop();
  const ports = useMemo(() => parsePorts(source), [source]);

  // Bottom dock resize: drag the splitter; clamp to a sane range.
  const startResize = (e: React.PointerEvent) => {
    const y0 = e.clientY;
    const h0 = bottomH;
    const move = (ev: PointerEvent) => setBottomH(Math.max(96, Math.min(window.innerHeight * 0.6, h0 + y0 - ev.clientY)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  const openSource = () => setDoc("source");

  return (
    <div className="app" style={{ "--bottom-h": `${bottomH}px` } as React.CSSProperties}>
      {/* ---- toolbar ---- */}
      <header className="toolbar">
        <div className="brand">
          <AppMark />
          <span className="wordmark">Formal DV</span>
        </div>
        <span className="tb-sep" />
        <span className="design-name" title="Top module">
          {session?.design.top ?? "no design"}
        </span>
        <span className="tb-sep" />
        <div className="tb-group">
          <button className="tbtn" disabled={busy || !scriptPath} onClick={runScript} title="Source the script path">
            <ScriptIcon /> Run script
          </button>
          <button
            className="tbtn accent"
            disabled={busy || !allProps.length}
            onClick={() => run(() => backend.evalTcl(proveCmd), proveCmd)}
            title={proveCmd}
          >
            <PlayIcon /> {busy ? "Running…" : "Prove all"}
          </button>
          <button className="tbtn" disabled title={busy ? "Stop is not wired to the backend yet" : "Nothing running"}>
            <StopIcon /> Stop
          </button>
        </div>
        <span className="tb-sep" />
        <label className="select-wrap" title="Tcl flavor">
          <select value={flavor} disabled={busy} onChange={(e) => changeFlavor(e.target.value as Flavor)} aria-label="Flavor">
            <option value="jg">JasperGold</option>
            <option value="vcf">VC Formal</option>
          </select>
          <ChevronIcon />
        </label>
        <input
          className="input mono path"
          placeholder="Script path, e.g. tests/fpv/counter/counter.jg.tcl"
          value={scriptPath}
          onChange={(e) => setScriptPath(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && runScript()}
          aria-label="Script path"
        />
        <span className="spacer" />
        {!backend.inTauri && <span className="badge-muted">Browser preview</span>}
        <button className="tbtn icon" onClick={cycleTheme} title={`Theme: ${theme} (click to cycle)`} aria-label="Toggle theme">
          {theme === "dark" ? <MoonIcon /> : theme === "light" ? <SunIcon /> : <AutoIcon />}
        </button>
      </header>

      <div className={`workbench ${left ? "" : "left-closed"}`}>
        {/* ---- icon rail ---- */}
        <nav className="rail" aria-label="Docks">
          <RailBtn label="Design" on={left === "design"} onClick={() => setLeft((l) => (l === "design" ? null : "design"))}>
            <TreeIcon />
          </RailBtn>
          <RailBtn label="Tasks" on={left === "tasks"} onClick={() => setLeft((l) => (l === "tasks" ? null : "tasks"))}>
            <ListIcon />
          </RailBtn>
        </nav>

        {/* ---- left dock ---- */}
        {left && (
          <aside className="dock left-dock">
            <div className="dock-tabs">
              <button className={`dtab ${left === "design" ? "on" : ""}`} onClick={() => setLeft("design")}>Design</button>
              <button className={`dtab ${left === "tasks" ? "on" : ""}`} onClick={() => setLeft("tasks")}>Tasks</button>
            </div>
            <div className="dock-body">
              {left === "design" ? (
                <DesignTree top={session?.design.top ?? null} files={session?.design.files ?? []} ports={ports} />
              ) : (
                <TaskList props={allProps} counts={counts} onPick={() => setStatusFilter(null)} />
              )}
            </div>
          </aside>
        )}

        {/* ---- centre documents ---- */}
        <main className="docs">
          <div className="doc-tabs" role="tablist">
            <DocTab on={doc === "properties"} onClick={() => setDoc("properties")}>
              Properties <span className="tab-count">{allProps.length}</span>
            </DocTab>
            <DocTab on={doc === "source"} onClick={() => setDoc("source")}>
              {fileName ?? "Source"}
            </DocTab>
            <DocTab on={false} disabled title="Waveforms arrive once engines export counterexample traces">
              Trace
            </DocTab>
          </div>

          {doc === "properties" ? (
            <div className="doc">
              <div className="doc-toolbar">
                <label className="search">
                  <SearchIcon />
                  <input placeholder="Filter properties" value={filter} onChange={(e) => setFilter(e.target.value)} />
                </label>
                <div className="pills" role="toolbar" aria-label="Filter by status">
                  {SUMMARY.map((s) => (
                    <button
                      key={s}
                      className={`pill ${statusFilter === s ? "on" : ""}`}
                      aria-pressed={statusFilter === s}
                      onClick={() => setStatusFilter((f) => (f === s ? null : s))}
                    >
                      <span className={`dot st-${s}`} />
                      {STATUS_LABEL[s]} <b>{counts[s] ?? 0}</b>
                    </button>
                  ))}
                </div>
              </div>
              <PropTable props={props} total={allProps.length} selected={selected} onSelect={setSelected} />
            </div>
          ) : (
            <div className="doc">
              <div className="doc-toolbar">
                <span className="mono muted">{topFile ?? "No source loaded"}</span>
              </div>
              <SourceView text={source} highlight={highlight} focus={focusLine} />
            </div>
          )}
        </main>

        {/* ---- right inspector ---- */}
        <aside className="dock right-dock">
          <div className="dock-head">Property</div>
          {current ? (
            <div className="dock-body inspector">
              <div className="insp-head">
                <span className="mono prop-name" title={current.name}>{current.name}</span>
                <StatusChip p={current} />
              </div>
              <pre className="code-block">{current.expr}</pre>
              <dl className="kv">
                <dt>Type</dt>
                <dd>{current.kind}</dd>
                <dt>Engine</dt>
                <dd className="mono">{current.status.engine ?? "—"}</dd>
                <dt>Result</dt>
                <dd>{result(current) || "—"}</dd>
                <dt>Time</dt>
                <dd className="mono">{time(current) || "—"}</dd>
                <dt>Source</dt>
                <dd>
                  {fileName ? (
                    <button className="link mono" onClick={openSource}>
                      {fileName}
                      {focusLine ? `:${focusLine}` : ""}
                    </button>
                  ) : (
                    "—"
                  )}
                </dd>
              </dl>
              <div className="insp-actions">
                {current.kind !== "assume" && (
                  <button
                    className="tbtn bordered"
                    disabled={busy}
                    onClick={() => run(() => backend.evalTcl(reproveCmd(current.name)), reproveCmd(current.name))}
                  >
                    <PlayIcon /> Re-prove
                  </button>
                )}
                <button
                  className="tbtn bordered"
                  disabled
                  title={
                    current.status.status === "cex" || current.status.status === "covered"
                      ? "Waveforms open once engines export counterexample traces"
                      : "No trace for this result"
                  }
                >
                  <WaveIcon /> Open trace
                </button>
              </div>
              {source && (
                <div className="preview">
                  <div className="preview-head">
                    <span>Source preview</span>
                    <button className="link mono" onClick={openSource}>{fileName}{focusLine ? `:${focusLine}` : ""}</button>
                  </div>
                  <SourceView text={source} highlight={[]} focus={focusLine} window={4} />
                </div>
              )}
            </div>
          ) : (
            <p className="empty-state">Select a property to see its expression and result.</p>
          )}
        </aside>

        {/* ---- bottom dock ---- */}
        <section className="dock bottom-dock">
          <div className="splitter" onPointerDown={startResize} role="separator" aria-orientation="horizontal" aria-label="Resize console" />
          <div className="dock-tabs">
            <button className={`dtab ${bottomTab === "console" ? "on" : ""}`} onClick={() => setBottomTab("console")}>Console</button>
            <button className={`dtab ${bottomTab === "log" ? "on" : ""}`} onClick={() => setBottomTab("log")}>
              Log {events.length > 0 && <span className="tab-count">{events.length}</span>}
            </button>
          </div>
          {bottomTab === "console" ? (
            <Console log={log} busy={busy} flavor={flavor} onSubmit={(cmd) => run(() => backend.evalTcl(cmd), cmd)} />
          ) : (
            <EventLog events={events} />
          )}
        </section>
      </div>

      {/* ---- status bar ---- */}
      <footer className="statusbar">
        <span>{flavor === "jg" ? "JasperGold" : "VC Formal"} flavor</span>
        <span>top <b className="mono">{session?.design.top ?? "—"}</b></span>
        <span>clock <b className="mono">{session?.clocks.map((c) => c.signal).join(", ") || "—"}</b></span>
        <span>reset <b className="mono">{session?.reset ?? "—"}</b></span>
        <span className="sb-counts">
          {SUMMARY.filter((s) => counts[s]).map((s) => (
            <span key={s}>
              <span className={`dot st-${s}`} /> {counts[s]}
            </span>
          ))}
        </span>
        <span className="spacer" />
        <span className={busy ? "running" : ""}>{busy ? "Running…" : "Idle"}</span>
      </footer>
    </div>
  );
}

// ---- helpers ---------------------------------------------------------------

function identifiers(expr: string): string[] {
  const skip = new Set(["past", "rose", "fell", "stable", "changed", "posedge", "negedge", "disable", "iff"]);
  return [...new Set(expr.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [])].filter((w) => !skip.has(w) && !/^\d/.test(w));
}

// Best-effort source line for a property: where its name appears, else the
// first line that mentions one of its signals. 1-based; 0 when unknown.
function locate(text: string, p: Property | null): number {
  if (!p || !text) return 0;
  const lines = text.split("\n");
  const byName = lines.findIndex((l) => new RegExp(`\\b${p.name}\\b`).test(l));
  if (byName >= 0) return byName + 1;
  const ids = identifiers(p.expr);
  if (!ids.length) return 0;
  const re = new RegExp(`\\b(${ids.join("|")})\\b`);
  return lines.findIndex((l) => re.test(l)) + 1;
}

// The snapshot carries no netlist, so ports come from a light scan of the
// top file's ANSI port list. Good enough for a hierarchy preview.
type Port = { dir: string; name: string; width: string };
function parsePorts(text: string): Port[] {
  const head = text.match(/\bmodule\s+\w+\s*(?:#\s*\([\s\S]*?\)\s*)?\(([\s\S]*?)\)\s*;/);
  if (!head) return [];
  const out: Port[] = [];
  let dir = "";
  let width = "";
  for (const seg of head[1].split(",")) {
    const m = seg
      .trim()
      .match(/^(input|output|inout)?\s*(?:wire|logic|reg|bit)?\s*(?:signed\s*)?(\[[^\]]+\])?\s*([A-Za-z_]\w*)$/);
    if (!m) continue;
    if (m[1]) {
      dir = m[1];
      width = m[2] ?? "";
    } else if (m[2]) width = m[2];
    if (dir) out.push({ dir, name: m[3], width });
  }
  return out;
}

// ---- components --------------------------------------------------------------

function RailBtn({ label, on, onClick, children }: { label: string; on: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button className={`rail-btn ${on ? "on" : ""}`} title={label} aria-label={label} aria-pressed={on} onClick={onClick}>
      {children}
    </button>
  );
}

function DocTab({ on, onClick, disabled, title, children }: { on: boolean; onClick?: () => void; disabled?: boolean; title?: string; children: React.ReactNode }) {
  return (
    <button role="tab" aria-selected={on} className={`doc-tab ${on ? "on" : ""}`} onClick={onClick} disabled={disabled} title={title}>
      {children}
    </button>
  );
}

function DesignTree({ top, files, ports }: { top: string | null; files: string[]; ports: Port[] }) {
  const [open, setOpen] = useState(true);
  if (!top) return <p className="empty-state">No design elaborated.</p>;
  return (
    <div className="tree">
      <button className="tree-row" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        <span className={`twisty ${open ? "open" : ""}`}><ChevronIcon /></span>
        <ModuleIcon />
        <span className="mono">{top}</span>
        <span className="muted tree-note">top</span>
      </button>
      {open &&
        ports.map((p) => (
          <div key={p.name} className="tree-row leaf" title={`${p.dir} ${p.width} ${p.name}`}>
            <span className={`port-dir dir-${p.dir}`}>{p.dir === "input" ? "in" : p.dir === "output" ? "out" : "io"}</span>
            <span className="mono">{p.name}</span>
            {p.width && <span className="muted mono tree-note">{p.width}</span>}
          </div>
        ))}
      {files.length > 0 && (
        <>
          <div className="tree-section">Files</div>
          {files.map((f) => (
            <div key={f} className="tree-row leaf" title={f}>
              <FileIcon />
              <span className="mono ellipsis">{f.split("/").pop()}</span>
            </div>
          ))}
        </>
      )}
    </div>
  );
}

function TaskList({ props, counts, onPick }: { props: Property[]; counts: Partial<Record<StatusWord, number>>; onPick: () => void }) {
  const asserts = props.filter((p) => p.kind === "assert").length;
  const covers = props.filter((p) => p.kind === "cover").length;
  const assumes = props.filter((p) => p.kind === "assume").length;
  return (
    <div className="tree">
      <button className="tree-row task on" onClick={onPick}>
        <ListIcon />
        <span className="mono">&lt;embedded&gt;</span>
        <span className="muted tree-note">{props.length}</span>
      </button>
      <div className="task-stats">
        <span>{asserts} assert</span>
        <span>{covers} cover</span>
        <span>{assumes} assume</span>
      </div>
      <div className="task-stats">
        {SUMMARY.filter((s) => counts[s]).map((s) => (
          <span key={s}>
            <span className={`dot st-${s}`} /> {counts[s]} {STATUS_LABEL[s]}
          </span>
        ))}
      </div>
    </div>
  );
}

function StatusLabel({ p }: { p: Property }) {
  const k = statusKey(p);
  return (
    <span className={`status st-${k}`}>
      <span className={`dot st-${k}`} />
      {statusText(p)}
    </span>
  );
}
function StatusChip({ p }: { p: Property }) {
  const k = statusKey(p);
  return (
    <span className={`chip st-${k}`}>
      <span className={`dot st-${k}`} />
      {statusText(p)}
    </span>
  );
}

const COLS: { key: SortKey; label: string; cls?: string }[] = [
  { key: "name", label: "Name" },
  { key: "kind", label: "Type" },
  { key: "status", label: "Status" },
  { key: "engine", label: "Engine" },
  { key: "result", label: "Result / Bound" },
  { key: "time", label: "Time", cls: "num" },
];

function sortValue(p: Property, k: SortKey): string | number {
  switch (k) {
    case "name": return p.name;
    case "kind": return p.kind;
    case "status": return p.kind === "assume" ? 99 : STATUS_ORDER.indexOf(p.status.status);
    case "engine": return p.status.engine ?? "";
    case "result": return result(p);
    case "time": return p.status.ms ?? -1;
  }
}

function PropTable({ props, total, selected, onSelect }: { props: Property[]; total: number; selected: string | null; onSelect: (n: string) => void }) {
  const [sort, setSort] = useState<{ key: SortKey; dir: 1 | -1 } | null>(null);
  const body = useRef<HTMLTableSectionElement>(null);
  const rows = useMemo(() => {
    if (!sort) return props;
    return [...props].sort((a, b) => {
      const x = sortValue(a, sort.key);
      const y = sortValue(b, sort.key);
      return (x < y ? -1 : x > y ? 1 : 0) * sort.dir;
    });
  }, [props, sort]);
  const onKey = (e: React.KeyboardEvent) => {
    if ((e.key !== "ArrowDown" && e.key !== "ArrowUp") || !rows.length) return;
    e.preventDefault();
    const i = rows.findIndex((p) => p.name === selected);
    const n = e.key === "ArrowDown" ? Math.min(i + 1, rows.length - 1) : Math.max(i - 1, 0);
    onSelect(rows[n].name);
    body.current?.children[n]?.scrollIntoView({ block: "nearest" });
  };
  return (
    <div className="table-wrap" tabIndex={0} onKeyDown={onKey} aria-label="Properties (arrow keys move the selection)">
      <table className="grid">
        <thead>
          <tr>
            {COLS.map((c) => (
              <th
                key={c.key}
                className={c.cls}
                aria-sort={sort?.key === c.key ? (sort.dir === 1 ? "ascending" : "descending") : "none"}
                onClick={() =>
                  setSort((s) => (s?.key !== c.key ? { key: c.key, dir: 1 } : s.dir === 1 ? { key: c.key, dir: -1 } : null))
                }
              >
                {c.label}
                <span className="sort">{sort?.key === c.key ? (sort.dir === 1 ? "▲" : "▼") : ""}</span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody ref={body}>
          {rows.map((p) => (
            <tr
              key={p.name}
              className={`${p.name === selected ? "sel" : ""} ${p.enabled ? "" : "off"}`}
              aria-selected={p.name === selected}
              onClick={() => onSelect(p.name)}
            >
              <td className="mono name">{p.name}</td>
              <td className="muted">{p.kind}</td>
              <td><StatusLabel p={p} /></td>
              <td className="mono muted">{p.status.engine ?? ""}</td>
              <td className="result">{result(p)}</td>
              <td className="mono num muted">{time(p)}</td>
            </tr>
          ))}
          {rows.length === 0 && (
            <tr className="empty-row">
              <td colSpan={COLS.length} className="empty">
                {total
                  ? "No properties match the current filter."
                  : "No properties yet. Run a JasperGold or VC Formal script, or type commands in the console."}
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

function Console({ log, busy, flavor, onSubmit }: { log: LogLine[]; busy: boolean; flavor: Flavor; onSubmit: (c: string) => void }) {
  const [cmd, setCmd] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [hi, setHi] = useState(-1);
  const end = useRef<HTMLDivElement>(null);
  const prompt = `fdv[${flavor}]>`;
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [log]);
  const submit = () => {
    if (!cmd.trim() || busy) return;
    onSubmit(cmd);
    setHistory((h) => [cmd, ...h]);
    setHi(-1);
    setCmd("");
  };
  return (
    <div className="console">
      <div className="console-log mono">
        {log.length === 0 && <div className="line hint">Type a Tcl command below. ↑/↓ walks history.</div>}
        {log.map((l, i) => (
          <div key={i} className={`line ${l.kind}`}>
            {l.kind === "in" ? (
              <>
                <span className="prompt">{prompt}</span> {l.text}
              </>
            ) : (
              l.text
            )}
          </div>
        ))}
        <div ref={end} />
      </div>
      <div className="composer">
        <span className="prompt mono">{prompt}</span>
        <input
          className="composer-input mono"
          value={cmd}
          disabled={busy}
          aria-label="Tcl command"
          placeholder="Tcl command, e.g. assert -name a1 {req |-> ##1 ack}"
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            else if (e.key === "ArrowUp" && history.length) {
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
        <div className="segmented" role="group" aria-label="Input mode">
          <button className="seg on" aria-pressed="true">Tcl</button>
          <button className="seg" disabled title="Ask: coming soon">Ask</button>
        </div>
        <button className="send" disabled={busy || !cmd.trim()} onClick={submit} aria-label="Run command" title="Run (Enter)">
          <ArrowUpIcon />
        </button>
      </div>
    </div>
  );
}

function EventLog({ events }: { events: Event[] }) {
  if (!events.length) return <p className="empty-state">No commands run yet.</p>;
  return (
    <div className="event-log">
      <table className="grid compact">
        <tbody>
          {[...events].reverse().map((e, i) => (
            <tr key={i}>
              <td className="mono muted">{e.at.toLocaleTimeString()}</td>
              <td>
                <span className={`status st-${e.ok ? "proven" : "error"}`}>
                  <span className={`dot st-${e.ok ? "proven" : "error"}`} />
                  {e.ok ? "ok" : "error"}
                </span>
              </td>
              <td className="mono">{e.cmd}</td>
              <td className="muted ellipsis">{e.summary}</td>
              <td className="mono num muted">{Math.round(e.ms)} ms</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// Lines that mention a signal of the selected property are marked, a
// first step toward cross-probing from property to RTL. `window` limits the
// view to a few lines around the focus line (inspector preview).
function SourceView({ text, highlight, focus, window: win }: { text: string; highlight: string[]; focus: number; window?: number }) {
  const re = highlight.length ? new RegExp(`\\b(${highlight.join("|")})\\b`) : null;
  const focusRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (win === undefined) focusRef.current?.scrollIntoView({ block: "center" });
  }, [focus, text, win]);
  const lines = text.split("\n");
  const from = win !== undefined && focus ? Math.max(0, focus - 1 - win) : 0;
  const to = win !== undefined ? (focus ? Math.min(lines.length, focus + win) : Math.min(lines.length, win * 2 + 1)) : lines.length;
  return (
    <pre className={`source ${win !== undefined ? "mini" : ""}`}>
      {lines.slice(from, to).map((line, j) => {
        const i = from + j;
        return (
          <div
            key={i}
            ref={i + 1 === focus ? focusRef : undefined}
            className={`${re && re.test(line) ? "hit" : ""} ${i + 1 === focus ? "focus" : ""}`}
          >
            <span className="ln">{i + 1}</span>
            <span className="code">{line || " "}</span>
          </div>
        );
      })}
    </pre>
  );
}

// ---- icons (inline SVG, currentColor) ----------------------------------------

function AppMark() {
  return (
    <svg className="app-mark" width={20} height={20} viewBox="0 0 24 24" aria-hidden="true">
      <rect x="1" y="1" width="22" height="22" rx="6" fill="var(--accent)" />
      <path d="M7 12.5l3 3 7-7" fill="none" stroke="var(--on-accent)" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
const ico = {
  width: 14,
  height: 14,
  viewBox: "0 0 16 16",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.6,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};
const PlayIcon = () => <svg {...ico}><path d="M5 3.5v9l7-4.5z" fill="currentColor" /></svg>;
const StopIcon = () => <svg {...ico}><rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor" /></svg>;
const ScriptIcon = () => <svg {...ico}><path d="M4 2.5h5.5L12 5v8.5H4z" /><path d="M6 8h4M6 10.5h4" /></svg>;
const SearchIcon = () => <svg {...ico}><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5L14 14" /></svg>;
const ChevronIcon = () => <svg {...ico} width={12} height={12}><path d="M4 6l4 4 4-4" /></svg>;
const ArrowUpIcon = () => <svg {...ico}><path d="M8 13V3M4 7l4-4 4 4" /></svg>;
const TreeIcon = () => <svg {...ico} width={18} height={18}><rect x="2" y="2" width="5" height="4" rx="1" /><rect x="9" y="7" width="5" height="3" rx="1" /><rect x="9" y="11.5" width="5" height="3" rx="1" /><path d="M4.5 6v7h4.5M4.5 8.5H9" /></svg>;
const ListIcon = () => <svg {...ico} width={16} height={16}><path d="M6 4h8M6 8h8M6 12h8" /><circle cx="2.8" cy="4" r=".8" fill="currentColor" /><circle cx="2.8" cy="8" r=".8" fill="currentColor" /><circle cx="2.8" cy="12" r=".8" fill="currentColor" /></svg>;
const ModuleIcon = () => <svg {...ico}><rect x="2.5" y="3.5" width="11" height="9" rx="1.5" /><path d="M5 3.5v-1.5M8 3.5v-1.5M11 3.5v-1.5M5 14v-1.5M8 14v-1.5M11 14v-1.5" /></svg>;
const FileIcon = () => <svg {...ico}><path d="M4 2.5h5.5L12 5v8.5H4z" /></svg>;
const WaveIcon = () => <svg {...ico}><path d="M1.5 11h3V5h3v6h3V5h4" /></svg>;
const SunIcon = () => <svg {...ico} width={16} height={16}><circle cx="8" cy="8" r="3" /><path d="M8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1" /></svg>;
const MoonIcon = () => <svg {...ico} width={16} height={16}><path d="M13 9.5A5.5 5.5 0 016.5 3a5.5 5.5 0 106.5 6.5z" /></svg>;
const AutoIcon = () => <svg {...ico} width={16} height={16}><circle cx="8" cy="8" r="5.5" /><path d="M8 2.5v11a5.5 5.5 0 000-11z" fill="currentColor" /></svg>;
