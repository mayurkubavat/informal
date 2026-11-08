// UI v2: a quiet EDA workbench over one fdv shell, laid out the way
// JasperGold / VC Formal users expect: menu bar, tool row, task tree on the
// left, property table in the centre, a bottom dock with the Tcl console,
// property details and messages, and a status bar. Colour is reserved for
// status glyphs; the frame stays neutral. Waveforms (Surfer) arrive behind
// "Visualize trace" once the engines export counterexample traces.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, Flavor, Reply, SessionView, StatusWord } from "./backend";
import {
  bound, locate, parsePorts, STATUS_LABEL, SUMMARY, TASK, statusKey, time,
  type BottomTab, type Center, type LeftTab, type LogEvent, type LogLine, type StatusFilter,
} from "./model";
import { useTheme } from "./useTheme";
import { MenuBar, type MenuDef } from "./components/MenuBar";
import { ToolBtn } from "./components/ToolBtn";
import { Tabs } from "./components/Tabs";
import { TaskTree } from "./components/TaskTree";
import { DesignTree } from "./components/DesignTree";
import { Status } from "./components/Glyph";
import { PropTable } from "./components/PropTable";
import { Console } from "./components/Console";
import { Messages } from "./components/Messages";
import { SourceView } from "./components/SourceView";
import { CodeIcon, PlayIcon, ScriptIcon, StopIcon, WaveIcon } from "./components/icons";

export default function App() {
  const [session, setSession] = useState<SessionView | null>(null);
  const [flavor, setFlavor] = useState<Flavor>("jg");
  const [scriptPath, setScriptPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [log, setLog] = useState<LogLine[]>([]);
  const [events, setEvents] = useState<LogEvent[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>(null);
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
                <TaskTree
                  total={allProps.length}
                  counts={counts}
                  assumes={assumes}
                  filter={statusFilter}
                  onFilter={setStatusFilter}
                  props={allProps}
                  selected={selected}
                  onSelect={setSelected}
                  onOpen={select}
                />
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
