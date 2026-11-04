// UI v2: a quiet EDA workbench over one fdv shell, laid out the way
// JasperGold / VC Formal users expect: menu bar, tool row, task tree on the
// left, property table in the centre, a bottom dock with the Tcl console,
// property details and messages, and a status bar. Colour is reserved for
// status glyphs; the frame stays neutral. Waveforms (Surfer) arrive behind
// "Visualize trace" once the engines export counterexample traces.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, Flavor, Property, Reply, SessionView, StatusWord } from "./backend";

type LogLine = { kind: "in" | "out" | "err"; text: string };
type Event = { at: Date; cmd: string; ok: boolean; ms: number; summary: string };
type Theme = "system" | "light" | "dark";
type Center = "table" | "source";
type BottomTab = "console" | "details" | "messages";
type LeftTab = "tasks" | "design";
type SortKey = "kind" | "name" | "status" | "engine" | "bound" | "time";

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
const KIND_LETTER = { assert: "A", cover: "C", assume: "M" } as const;
const TASK = "<embedded>";

function bound(p: Property): string {
  const s = p.status;
  switch (s.status) {
    case "cex":
    case "covered":
      return String(s.depth ?? "?");
    case "proven":
    case "unreachable":
      return "∞";
    case "undetermined":
      return s.reason ?? "";
    case "error":
      return (s.message ?? "").split("\n")[0];
    default:
      return "";
  }
}
function time(p: Property): string {
  return p.status.ms !== undefined ? `${(p.status.ms / 1000).toFixed(2)}s` : "";
}
function statusKey(p: Property): string {
  if (p.kind === "assume") return "assume";
  return p.enabled ? p.status.status : "unprocessed";
}
function statusText(p: Property): string {
  if (p.kind === "assume") return "Assumed";
  return p.enabled ? STATUS_LABEL[p.status.status] : "Disabled";
}

function useTheme(): [Theme, (t: Theme) => void] {
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
  return [theme, setTheme];
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
  const [statusFilter, setStatusFilter] = useState<StatusWord | "assume" | null>(null);
  const [source, setSource] = useState("");
  const [center, setCenter] = useState<Center>("table");
  const [left, setLeft] = useState<LeftTab>("tasks");
  const [leftOpen, setLeftOpen] = useState(true);
  const [bottomTab, setBottomTab] = useState<BottomTab>("console");
  const [bottomH, setBottomH] = useState(250);
  const [theme, setTheme] = useTheme();
  const pathRef = useRef<HTMLInputElement>(null);

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
        setEvents((e) => [...e, { at: new Date(), cmd: echo, ok, ms: performance.now() - t0, summary: text.split("\n")[0] }]);
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

  // Recomputed every render (cheap): the browser mock mutates the session in
  // place, so memoising on the session reference would go stale.
  const allProps = session?.props ?? [];
  const props = allProps.filter(
    (p) =>
      (p.name.includes(filter) || p.expr.includes(filter)) &&
      (!statusFilter || statusKey(p) === statusFilter || (statusFilter !== "assume" && p.kind !== "assume" && p.status.status === statusFilter)),
  );
  const counts: Partial<Record<StatusWord, number>> = {};
  for (const p of allProps) if (p.kind !== "assume") counts[p.status.status] = (counts[p.status.status] ?? 0) + 1;
  const assumes = allProps.filter((p) => p.kind === "assume").length;
  const unprocessed = counts.unprocessed ?? 0;

  const current = allProps.find((p) => p.name === selected) ?? null;
  const proveCmd = flavor === "jg" ? "prove -all" : "check_fv";
  const proveOne = (n: string) => (flavor === "jg" ? `prove -property ${n}` : `check_fv -property {${n}}`);
  const focusLine = locate(source, current);
  const fileName = topFile?.split("/").pop();
  const ports = useMemo(() => parsePorts(source), [source]);
  const hasTrace = !!current && (current.status.status === "cex" || current.status.status === "covered");

  const proveAll = () => allProps.length && !busy && run(() => backend.evalTcl(proveCmd), proveCmd);
  const proveSelected = () => current && current.kind !== "assume" && !busy && run(() => backend.evalTcl(proveOne(current.name)), proveOne(current.name));
  const runScript = () => {
    if (!scriptPath) {
      pathRef.current?.focus();
      return;
    }
    if (!busy) run(() => backend.source(scriptPath), `source ${scriptPath}`);
  };
  const changeFlavor = (f: Flavor) => {
    setFlavor(f);
    run(() => backend.setFlavor(f));
  };
  const select = (n: string) => {
    setSelected(n);
    setBottomTab("details");
  };

  const summary = [
    `${allProps.length} properties`,
    ...SUMMARY.filter((s) => counts[s]).map((s) => `${counts[s]} ${STATUS_LABEL[s].toLowerCase()}`),
    ...(unprocessed ? [`${unprocessed} unprocessed`] : []),
    ...(assumes ? [`${assumes} assumed`] : []),
  ].join(" · ");

  const startResize = (e: React.PointerEvent) => {
    const y0 = e.clientY;
    const h0 = bottomH;
    const move = (ev: PointerEvent) => setBottomH(Math.max(120, Math.min(window.innerHeight * 0.65, h0 + y0 - ev.clientY)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  const menus: MenuDef[] = [
    {
      label: "File",
      items: [
        { label: "Source script…", onClick: runScript },
        { label: "Edit script path", onClick: () => pathRef.current?.focus() },
        { sep: true },
        { label: "Close design", disabled: true },
      ],
    },
    { label: "Edit", items: [{ label: "Find property", onClick: () => document.getElementById("prop-filter")?.focus() }, { label: "Clear filters", onClick: () => { setFilter(""); setStatusFilter(null); } }] },
    {
      label: "View",
      items: [
        { label: "Task / Design dock", checked: leftOpen, onClick: () => setLeftOpen((o) => !o) },
        { label: "Property table", checked: center === "table", onClick: () => setCenter("table") },
        { label: `Source (${fileName ?? "none"})`, checked: center === "source", onClick: () => setCenter("source"), disabled: !topFile },
        { sep: true },
        { label: "Console", onClick: () => setBottomTab("console") },
        { label: "Messages", onClick: () => setBottomTab("messages") },
        { sep: true },
        { label: "Input mode: Tcl", checked: true },
        { label: "Input mode: Ask (coming soon)", disabled: true },
        { sep: true },
        { label: "Theme: System", checked: theme === "system", onClick: () => setTheme("system") },
        { label: "Theme: Light", checked: theme === "light", onClick: () => setTheme("light") },
        { label: "Theme: Dark", checked: theme === "dark", onClick: () => setTheme("dark") },
      ],
    },
    { label: "Design", items: [{ label: "Show hierarchy", onClick: () => { setLeftOpen(true); setLeft("design"); } }, { label: "Elaborate", disabled: true }] },
    {
      label: "Prove",
      items: [
        { label: "Prove all", hint: proveCmd, onClick: proveAll, disabled: busy || !allProps.length },
        { label: "Prove selected", onClick: proveSelected, disabled: busy || !current || current.kind === "assume" },
        { label: "Stop", disabled: true },
      ],
    },
    {
      label: "Debug",
      items: [
        { label: "Visualize trace", disabled: true },
        { label: "Show source of selected", onClick: () => current && setBottomTab("details"), disabled: !current },
      ],
    },
    { label: "Tools", items: [{ label: "Proof profiler (coming soon)", disabled: true }] },
    { label: "Help", items: [{ label: "Formal DV v0.1", disabled: true }] },
  ];

  return (
    <div className="app" style={{ "--bottom-h": `${bottomH}px` } as React.CSSProperties}>
      {/* ---- menu bar ---- */}
      <MenuBar menus={menus}>
        <input
          ref={pathRef}
          className="menu-path mono"
          placeholder="script path, e.g. tests/fpv/counter/counter.jg.tcl"
          value={scriptPath}
          onChange={(e) => setScriptPath(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && runScript()}
          aria-label="Script path"
          title="Script path (Enter to source)"
        />
      </MenuBar>

      {/* ---- tool row ---- */}
      <div className="toolrow">
        <label className="field">
          <span>App</span>
          <select aria-label="App" defaultValue="fpv">
            <option value="fpv">FPV</option>
            <option disabled>SEC (later)</option>
            <option disabled>COV (later)</option>
          </select>
        </label>
        <label className="field">
          <span>Flavor</span>
          <select value={flavor} disabled={busy} onChange={(e) => changeFlavor(e.target.value as Flavor)} aria-label="Flavor">
            <option value="jg">JasperGold</option>
            <option value="vcf">VC Formal</option>
          </select>
        </label>
        <span className="vsep" />
        <ToolBtn title={scriptPath ? `source ${scriptPath}` : "Run script (set a script path first)"} onClick={runScript} disabled={busy}>
          <ScriptIcon />
        </ToolBtn>
        <button className="tool tool-text" onClick={proveAll} disabled={busy || !allProps.length} title={proveCmd}>
          <PlayIcon /> {busy ? "Running…" : "Prove all"}
        </button>
        <ToolBtn title="Stop (not wired to the backend yet)" disabled>
          <StopIcon />
        </ToolBtn>
        <span className="vsep" />
        <ToolBtn title={hasTrace ? "Visualize trace (coming soon)" : "Visualize trace: select a CEX or cover"} disabled>
          <WaveIcon />
        </ToolBtn>
        <ToolBtn title="Source view" onClick={() => setCenter((c) => (c === "source" ? "table" : "source"))} disabled={!topFile} on={center === "source"}>
          <CodeIcon />
        </ToolBtn>
        <span className="spacer" />
        {!backend.inTauri && <span className="muted small">Browser preview</span>}
      </div>

      <div className={`workbench ${leftOpen ? "" : "left-closed"}`}>
        {/* ---- left dock ---- */}
        {leftOpen && (
          <aside className="left">
            <Tabs
              tabs={[
                ["tasks", "Tasks"],
                ["design", "Design"],
              ]}
              value={left}
              onChange={(t) => setLeft(t as LeftTab)}
            />
            <div className="pane-body">
              {left === "tasks" ? (
                <TaskTree total={allProps.length} counts={counts} assumes={assumes} filter={statusFilter} onFilter={setStatusFilter} />
              ) : (
                <DesignTree top={session?.design.top ?? null} files={session?.design.files ?? []} ports={ports} />
              )}
            </div>
          </aside>
        )}

        {/* ---- centre ---- */}
        <main className="center">
          <div className="strip">
            <Tabs
              tabs={[
                ["table", "Property Table"],
                ...(topFile ? ([["source", fileName ?? "Source"]] as [string, string][]) : []),
              ]}
              value={center}
              onChange={(t) => setCenter(t as Center)}
            />
            {center === "table" ? (
              <>
                <span className="strip-summary">{summary}</span>
                <span className="spacer" />
                {statusFilter && (
                  <button className="link small" onClick={() => setStatusFilter(null)}>
                    clear status filter
                  </button>
                )}
                <input id="prop-filter" className="filter" placeholder="Filter" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter properties" />
              </>
            ) : (
              <span className="strip-summary mono">{topFile}</span>
            )}
          </div>
          {center === "table" ? (
            <PropTable
              props={props}
              total={allProps.length}
              selected={selected}
              onSelect={select}
              onOpen={(p) => {
                select(p.name);
              }}
            />
          ) : (
            <SourceView text={source} focus={focusLine} />
          )}
        </main>

        {/* ---- bottom dock ---- */}
        <section className="bottom">
          <div className="splitter" onPointerDown={startResize} role="separator" aria-orientation="horizontal" aria-label="Resize bottom dock" />
          <Tabs
            tabs={[
              ["console", "Console"],
              ["details", current ? `Details: ${current.name}` : "Details"],
              ["messages", `Messages${events.some((e) => !e.ok) ? ` (${events.filter((e) => !e.ok).length})` : ""}`],
            ]}
            value={bottomTab}
            onChange={(t) => setBottomTab(t as BottomTab)}
          />
          {/* kept mounted so history and scroll survive tab switches */}
          <Console hidden={bottomTab !== "console"} log={log} busy={busy} flavor={flavor} onSubmit={(cmd) => run(() => backend.evalTcl(cmd), cmd)} />
          {bottomTab === "details" &&
            (current ? (
              <div className="details">
                <div className="details-left">
                  <div className="details-title">
                    <span className="mono strong">{current.name}</span>
                    <Status p={current} />
                  </div>
                  <pre className="expr">{current.expr}</pre>
                  <dl className="kv">
                    <dt>Type</dt>
                    <dd>{current.kind}</dd>
                    <dt>Engine</dt>
                    <dd className="mono">{current.status.engine ?? "—"}</dd>
                    <dt>Bound</dt>
                    <dd className="mono">{bound(current) || "—"}</dd>
                    <dt>Time</dt>
                    <dd className="mono">{time(current) || "—"}</dd>
                    <dt>Task</dt>
                    <dd className="mono">{TASK}</dd>
                    <dt>Source</dt>
                    <dd>
                      {fileName ? (
                        <button className="link mono" onClick={() => setCenter("source")}>
                          {fileName}
                          {focusLine ? `:${focusLine}` : ""}
                        </button>
                      ) : (
                        "—"
                      )}
                    </dd>
                  </dl>
                  <div className="details-actions">
                    {current.kind !== "assume" && (
                      <button className="btn" disabled={busy} onClick={proveSelected}>
                        Re-prove
                      </button>
                    )}
                    <button className="btn" disabled title={hasTrace ? "Coming soon" : "No trace for this result"}>
                      Visualize trace
                    </button>
                  </div>
                </div>
                <div className="details-right">
                  <SourceView text={source} focus={focusLine} />
                </div>
              </div>
            ) : (
              <p className="empty">Select a property to see its expression, result and source.</p>
            ))}
          {bottomTab === "messages" && <Messages events={events} />}
        </section>
      </div>

      {/* ---- status bar ---- */}
      <footer className="statusbar">
        <span>top: <span className="mono">{session?.design.top ?? "—"}</span></span>
        <span>clock: <span className="mono">{session?.clocks.map((c) => c.signal).join(", ") || "—"}</span></span>
        <span>reset: <span className="mono">{session?.reset ?? "—"}</span></span>
        <span>{flavor === "jg" ? "JasperGold" : "VC Formal"} flavor</span>
        <span className="spacer" />
        <span>{busy ? "Running…" : "Idle"}</span>
      </footer>
    </div>
  );
}

// ---- helpers -----------------------------------------------------------------

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
    const m = seg.trim().match(/^(input|output|inout)?\s*(?:wire|logic|reg|bit)?\s*(?:signed\s*)?(\[[^\]]+\])?\s*([A-Za-z_]\w*)$/);
    if (!m) continue;
    if (m[1]) {
      dir = m[1];
      width = m[2] ?? "";
    } else if (m[2]) width = m[2];
    if (dir) out.push({ dir, name: m[3], width });
  }
  return out;
}

// ---- components ----------------------------------------------------------------

type MenuItem = { label: string; hint?: string; onClick?: () => void; disabled?: boolean; checked?: boolean } | { sep: true };
type MenuDef = { label: string; items: MenuItem[] };

function MenuBar({ menus, children }: { menus: MenuDef[]; children?: React.ReactNode }) {
  const [open, setOpen] = useState<string | null>(null);
  const bar = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !bar.current?.contains(e.target as Node) && setOpen(null);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(null);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  return (
    <div className="menubar" ref={bar} role="menubar">
      {menus.map((m) => (
        <div key={m.label} className="menu">
          <button
            className={`menu-title ${open === m.label ? "on" : ""}`}
            role="menuitem"
            aria-haspopup="true"
            aria-expanded={open === m.label}
            onClick={() => setOpen((o) => (o === m.label ? null : m.label))}
            onMouseEnter={() => open && setOpen(m.label)}
          >
            {m.label}
          </button>
          {open === m.label && (
            <div className="menu-pop" role="menu">
              {m.items.map((it, i) =>
                "sep" in it ? (
                  <div key={i} className="menu-sep" />
                ) : (
                  <button
                    key={i}
                    role="menuitem"
                    className="menu-item"
                    disabled={it.disabled}
                    onClick={() => {
                      setOpen(null);
                      it.onClick?.();
                    }}
                  >
                    <span className="menu-check">{it.checked ? "✓" : ""}</span>
                    <span>{it.label}</span>
                    {it.hint && <span className="menu-hint mono">{it.hint}</span>}
                  </button>
                ),
              )}
            </div>
          )}
        </div>
      ))}
      <span className="spacer" />
      {children}
    </div>
  );
}

function ToolBtn({ title, onClick, disabled, on, children }: { title: string; onClick?: () => void; disabled?: boolean; on?: boolean; children: React.ReactNode }) {
  return (
    <button className={`tool ${on ? "on" : ""}`} title={title} aria-label={title} onClick={onClick} disabled={disabled}>
      {children}
    </button>
  );
}

function Tabs({ tabs, value, onChange }: { tabs: [string, string][]; value: string; onChange: (v: string) => void }) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map(([k, label]) => (
        <button key={k} role="tab" aria-selected={value === k} className={`tab ${value === k ? "on" : ""}`} onClick={() => onChange(k)}>
          {label}
        </button>
      ))}
    </div>
  );
}

function TaskTree({
  total,
  counts,
  assumes,
  filter,
  onFilter,
}: {
  total: number;
  counts: Partial<Record<StatusWord, number>>;
  assumes: number;
  filter: StatusWord | "assume" | null;
  onFilter: (f: StatusWord | "assume" | null) => void;
}) {
  const [open, setOpen] = useState(true);
  const groups: [StatusWord | "assume", string, number][] = [
    ...SUMMARY.map((s) => [s, STATUS_LABEL[s], counts[s] ?? 0] as [StatusWord, string, number]),
    ["unprocessed", "Unprocessed", counts.unprocessed ?? 0],
    ["assume", "Assumed", assumes],
  ];
  return (
    <div className="tree" role="tree">
      <div className={`tree-row ${filter === null ? "sel" : ""}`} role="treeitem" aria-expanded={open} onClick={() => onFilter(null)}>
        <button className={`twisty ${open ? "open" : ""}`} onClick={(e) => { e.stopPropagation(); setOpen((o) => !o); }} aria-label="Expand">
          <ChevronIcon />
        </button>
        <span className="mono">{TASK}</span>
        <span className="tree-count">{total}</span>
      </div>
      {open &&
        groups
          .filter(([, , n]) => n > 0)
          .map(([k, label, n]) => (
            <div
              key={k}
              role="treeitem"
              className={`tree-row child ${filter === k ? "sel" : ""}`}
              onClick={() => onFilter(filter === k ? null : k)}
            >
              <Glyph k={k} />
              <span>{label}</span>
              <span className="tree-count">{n}</span>
            </div>
          ))}
    </div>
  );
}

function DesignTree({ top, files, ports }: { top: string | null; files: string[]; ports: Port[] }) {
  const [open, setOpen] = useState(true);
  if (!top) return <p className="empty">No design elaborated.</p>;
  return (
    <div className="tree" role="tree">
      <div className="tree-row" role="treeitem" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <span className={`twisty ${open ? "open" : ""}`}><ChevronIcon /></span>
        <span className="mono">{top}</span>
        <span className="tree-count">top</span>
      </div>
      {open &&
        ports.map((p) => (
          <div key={p.name} className="tree-row child" role="treeitem" title={`${p.dir} ${p.width} ${p.name}`}>
            <span className="port-dir mono">{p.dir === "input" ? "in" : p.dir === "output" ? "out" : "io"}</span>
            <span className="mono">{p.name}</span>
            {p.width && <span className="tree-count mono">{p.width}</span>}
          </div>
        ))}
      {files.length > 0 && (
        <>
          <div className="tree-section">Files</div>
          {files.map((f) => (
            <div key={f} className="tree-row child" title={f}>
              <span className="mono ellipsis">{f}</span>
            </div>
          ))}
        </>
      )}
    </div>
  );
}

// Small status glyphs, the only strong colour in the window.
function Glyph({ k }: { k: string }) {
  const c = `glyph st-${k}`;
  const props = { className: c, width: 12, height: 12, viewBox: "0 0 12 12", "aria-hidden": true, fill: "none", stroke: "currentColor", strokeWidth: 1.6, strokeLinecap: "round" as const, strokeLinejoin: "round" as const };
  switch (k) {
    case "proven":
      return <svg {...props}><path d="M2.5 6.3l2.3 2.3 4.7-5" /></svg>;
    case "cex":
      return <svg {...props}><path d="M3 3l6 6M9 3l-6 6" /></svg>;
    case "covered":
      return <svg {...props}><circle cx="6" cy="6" r="4" /><circle cx="6" cy="6" r="1.5" fill="currentColor" stroke="none" /></svg>;
    case "unreachable":
      return <svg {...props}><circle cx="6" cy="6" r="4" /><path d="M3.2 8.8l5.6-5.6" /></svg>;
    case "undetermined":
      return <svg {...props}><path d="M4.3 4.4a1.8 1.8 0 113 1.4c-.8.5-1.3.8-1.3 1.7" /><circle cx="6" cy="9.6" r=".5" fill="currentColor" /></svg>;
    case "error":
      return <svg {...props}><path d="M6 1.8l4.6 8.2H1.4z" /><path d="M6 5v2.2" /><circle cx="6" cy="8.6" r=".4" fill="currentColor" /></svg>;
    case "assume":
      return <svg {...props}><path d="M3 6h6" /></svg>;
    default:
      return <svg {...props}><circle cx="6" cy="6" r="3.5" /></svg>;
  }
}

function Status({ p }: { p: Property }) {
  return (
    <span className="status">
      <Glyph k={statusKey(p)} />
      {statusText(p)}
    </span>
  );
}

const COLS: { key: SortKey | null; label: string; cls?: string }[] = [
  { key: "kind", label: "", cls: "c-kind" },
  { key: "name", label: "Name" },
  { key: "status", label: "Status" },
  { key: "engine", label: "Engine" },
  { key: "bound", label: "Bound", cls: "num" },
  { key: "time", label: "Time", cls: "num" },
  { key: null, label: "Task" },
];

function sortValue(p: Property, k: SortKey): string | number {
  switch (k) {
    case "kind": return p.kind;
    case "name": return p.name;
    case "status": return p.kind === "assume" ? 99 : STATUS_ORDER.indexOf(p.status.status);
    case "engine": return p.status.engine ?? "";
    case "bound": return p.status.depth ?? (p.status.status === "proven" || p.status.status === "unreachable" ? 1e9 : -1);
    case "time": return p.status.ms ?? -1;
  }
}

function PropTable({
  props,
  total,
  selected,
  onSelect,
  onOpen,
}: {
  props: Property[];
  total: number;
  selected: string | null;
  onSelect: (n: string) => void;
  onOpen: (p: Property) => void;
}) {
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
    <div className="table-wrap" tabIndex={0} onKeyDown={onKey} aria-label="Property table (arrow keys move the selection)">
      <table className="grid">
        <thead>
          <tr>
            {COLS.map((c) => (
              <th
                key={c.label || "kind"}
                className={c.cls}
                aria-sort={c.key && sort?.key === c.key ? (sort.dir === 1 ? "ascending" : "descending") : undefined}
                onClick={() =>
                  c.key &&
                  setSort((s) => (s?.key !== c.key ? { key: c.key!, dir: 1 } : s.dir === 1 ? { key: c.key!, dir: -1 } : null))
                }
                title={c.key === "kind" ? "Type: A assert, C cover, M assume" : undefined}
              >
                {c.label}
                {c.key && sort?.key === c.key && <span className="sort">{sort.dir === 1 ? "▴" : "▾"}</span>}
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
              onDoubleClick={() => onOpen(p)}
            >
              <td className="c-kind mono" title={p.kind}>{KIND_LETTER[p.kind]}</td>
              <td className="mono">{p.name}</td>
              <td><Status p={p} /></td>
              <td className="mono dim">{p.status.engine ?? ""}</td>
              <td className="mono num dim" title={bound(p)}>{bound(p)}</td>
              <td className="mono num dim">{time(p)}</td>
              <td className="mono dim">{TASK}</td>
            </tr>
          ))}
          {rows.length === 0 && (
            <tr className="empty-row">
              <td colSpan={COLS.length} className="empty">
                {total ? "No properties match the current filter." : "No properties. Source a JasperGold or VC Formal script, or type commands in the console."}
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

function Console({ hidden, log, busy, flavor, onSubmit }: { hidden: boolean; log: LogLine[]; busy: boolean; flavor: Flavor; onSubmit: (c: string) => void }) {
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

function Messages({ events }: { events: Event[] }) {
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

// Source with line numbers; only the property's own line is highlighted.
function SourceView({ text, focus }: { text: string; focus: number }) {
  const focusRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    focusRef.current?.scrollIntoView({ block: "center" });
  }, [focus, text]);
  if (!text) return <p className="empty">No source loaded.</p>;
  return (
    <pre className="source">
      {text.split("\n").map((line, i) => (
        <div key={i} ref={i + 1 === focus ? focusRef : undefined} className={i + 1 === focus ? "focus" : ""}>
          <span className="ln">{i + 1}</span>
          {line || " "}
        </div>
      ))}
    </pre>
  );
}

// ---- icons (inline SVG, currentColor) ------------------------------------------

const ico = {
  width: 16,
  height: 16,
  viewBox: "0 0 16 16",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};
const PlayIcon = () => <svg {...ico} width={12} height={12} className="play"><path d="M4 2.5v11l9-5.5z" fill="currentColor" stroke="none" /></svg>;
const StopIcon = () => <svg {...ico}><rect x="4" y="4" width="8" height="8" rx="1" /></svg>;
const ScriptIcon = () => <svg {...ico}><path d="M4 2h5.5L12.5 5v9H4z" /><path d="M9.5 2v3h3M6 8h4.5M6 10.5h4.5" /></svg>;
const WaveIcon = () => <svg {...ico}><path d="M1.5 11.5h2.5v-7h3v7h3v-7h2.5v7h2" /></svg>;
const CodeIcon = () => <svg {...ico}><path d="M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5" /></svg>;
const ChevronIcon = () => <svg {...ico} width={10} height={10}><path d="M4 6l4 4 4-4" /></svg>;
