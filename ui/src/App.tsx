// UI v2: a quiet EDA workbench over one fdv shell, laid out the way
// JasperGold / VC Formal users expect: menu bar, tool row, task tree on the
// left, property table in the centre, a bottom dock with the Tcl console,
// property details and messages, and a status bar. Colour is reserved for
// status glyphs; the frame stays neutral. Waveforms (Surfer) arrive behind
// "Visualize trace" once the engines export counterexample traces.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, Flavor, ProveProgress, Reply, SessionView } from "./backend";
import {
  bound, clock, isProveCmd, locate, matches, parsePorts, STATUS_LABEL, SUMMARY, TASK, statusKey, time, withProgress,
  type BottomTab, type Center, type LeftTab, type LogEvent, type LogLine, type StatusFilter, type ViewStatus,
} from "./model";
import { useTheme } from "./useTheme";
import { BOTTOM_DEFAULT, clampBottom, clampLeft, LEFT_MAX, LEFT_MIN, useLayout } from "./useLayout";
import { MenuBar, type MenuDef, type MenuItem } from "./components/MenuBar";
import { ContextMenu } from "./components/ContextMenu";
import { ToolBtn } from "./components/ToolBtn";
import { Tabs } from "./components/Tabs";
import { TaskTree } from "./components/TaskTree";
import { DesignTree } from "./components/DesignTree";
import { Glyph, Status } from "./components/Glyph";
import { PropTable } from "./components/PropTable";
import { Console } from "./components/Console";
import { Messages } from "./components/Messages";
import { SourceView } from "./components/SourceView";
import { CodeIcon, PlayIcon, ScriptIcon, StopIcon, WaveIcon } from "./components/icons";

const SHORTCUTS: [string, string][] = [
  ["F5", "Prove all"],
  ["F6", "Prove selected"],
  ["F3", "Toggle source view"],
  ["Enter", "Open details of the selected property (table)"],
  ["Up / Down", "Move the selection (table)"],
  ["Shift+F10", "Row actions menu (table)"],
  ["Esc", "Close menu; otherwise stop the running prove"],
  ["Ctrl+F", "Filter properties"],
  ["Ctrl+L", "Console prompt"],
  ["Ctrl+1..3", "Console / Details / Messages"],
];

export default function App() {
  const [session, setSession] = useState<SessionView | null>(null);
  const [flavor, setFlavor] = useState<Flavor>("jg");
  const [scriptPath, setScriptPath] = useState("");
  // Requests in flight other than a prove: they disable Prove, not the console.
  const [pending, setPending] = useState(0);
  const busy = pending > 0;
  // Our prove call is in flight (set before its first progress event).
  const [proving, setProving] = useState(false);
  const provingRef = useRef(false);
  const [progress, setProgress] = useState<ProveProgress | null>(null);
  const [stopping, setStopping] = useState(false);
  const [log, setLog] = useState<LogLine[]>([]);
  const [events, setEvents] = useState<LogEvent[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>(null);
  const [source, setSource] = useState("");
  const { layout, set } = useLayout();
  const { center, left, leftOpen, bottom: bottomTab, bottomH, leftW } = layout;
  const [setCenter, setLeft, setLeftOpen, setBottomTab, setBottomH, setLeftW] = useMemo(
    () => [set("center"), set("left"), set("leftOpen"), set("bottom"), set("bottomH"), set("leftW")],
    [set],
  );
  const [theme, setTheme] = useTheme();
  const [focusReq, setFocusReq] = useState<{ id: string; n: number } | null>(null);
  const [ctx, setCtx] = useState<{ name: string; x: number; y: number } | null>(null);
  const closeCtx = useCallback(() => setCtx(null), []);
  const pathRef = useRef<HTMLInputElement>(null);

  const apply = useCallback((r: Reply) => {
    if (r.session) setSession(r.session);
    if (r.result) setLog((l) => [...l, { kind: r.ok ? "out" : "err", text: r.result }]);
  }, []);

  // One backend request. The command is echoed up front so live console
  // lines from a prove land under it; the result follows when it returns.
  const run = useCallback(
    async (f: () => Promise<Reply>, echo?: string, isProve = false) => {
      if (!isProve) setPending((n) => n + 1);
      if (echo) setLog((l) => [...l, { kind: "in", text: echo }]);
      const t0 = performance.now();
      const note = (ok: boolean, text: string) =>
        echo &&
        setEvents((e) => [...e, { at: new Date(), cmd: echo, ok, ms: performance.now() - t0, summary: text.split("\n")[0] }]);
      try {
        const r = await f();
        apply(r);
        note(r.ok, r.result);
      } catch (e) {
        setLog((l) => [...l, { kind: "err", text: String(e) }]);
        note(false, String(e));
      } finally {
        if (!isProve) setPending((n) => n - 1);
      }
    },
    [apply],
  );

  // Live progress and console lines pushed by the backend during a prove.
  useEffect(() => {
    const offProgress = backend.onProgress((p) => {
      setProgress(p);
      if (p.kind === "done" || p.kind === "stopped") setStopping(false);
    });
    const offConsole = backend.onConsole((l) => setLog((log) => [...log, { kind: l.kind, text: l.text }]));
    return () => {
      offProgress();
      offConsole();
    };
  }, []);
  const live = progress?.kind === "start" || progress?.kind === "update";
  const running = proving || live;
  // The Running group disappears with the run; do not leave its filter behind.
  useEffect(() => {
    if (!running) setStatusFilter((f) => (f === "running" ? null : f));
  }, [running]);

  // One prove at a time: a second one (button, key or console) is refused.
  const prove = async (cmd: string) => {
    if (provingRef.current || live) {
      setLog((l) => [...l, { kind: "in", text: cmd }, { kind: "err", text: "a prove is already running; stop it first (Esc)" }]);
      return;
    }
    provingRef.current = true;
    setProving(true);
    setProgress(null);
    try {
      await run(() => backend.evalTcl(cmd), cmd, true);
    } finally {
      provingRef.current = false;
      setProving(false);
      setProgress(null);
      setStopping(false);
    }
  };
  const stop = () => {
    if (!running || stopping) return;
    setStopping(true);
    backend
      .stop()
      .then((r) => !r.ok && setLog((l) => [...l, { kind: "err", text: r.result }]))
      .catch((e) => {
        setStopping(false);
        setLog((l) => [...l, { kind: "err", text: `stop failed: ${e}` }]);
      });
  };

  useEffect(() => {
    run(() => backend.snapshot());
  }, [run]);

  const topFile = session?.design.files[0];
  useEffect(() => {
    if (topFile) backend.readText(topFile).then(setSource, () => setSource(""));
  }, [topFile]);

  // Recomputed every render (cheap): the browser mock mutates the session in
  // place, so memoising on the session reference would go stale.
  // While a prove runs, its progress overlays the session (running rows,
  // k-bound, elapsed); the reply's session takes over when it returns.
  const allProps = withProgress(session?.props ?? [], live || proving ? progress : null);
  const props = allProps.filter(
    (p) =>
      matches(p, filter) &&
      (!statusFilter || statusKey(p) === statusFilter || (statusFilter !== "assume" && p.kind !== "assume" && p.status.status === statusFilter)),
  );
  const counts: Partial<Record<ViewStatus, number>> = {};
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

  const idle = !busy && !running;
  const proveAll = () => allProps.length && idle && prove(proveCmd);
  const proveSelected = () => current && current.kind !== "assume" && idle && prove(proveOne(current.name));
  const runScript = () => {
    if (!scriptPath) {
      pathRef.current?.focus();
      return;
    }
    if (idle) run(() => backend.source(scriptPath), `source ${scriptPath}`);
  };
  const changeFlavor = (f: Flavor) => {
    setFlavor(f);
    run(() => backend.setFlavor(f));
  };
  const select = (n: string) => {
    setSelected(n);
    setBottomTab("details");
  };
  const copy = (text: string, what: string) =>
    navigator.clipboard
      .writeText(text)
      .then(() => setEvents((e) => [...e, { at: new Date(), cmd: "copy", ok: true, ms: 0, summary: `Copied ${what}` }]))
      .catch((err) => setLog((l) => [...l, { kind: "err", text: `copy failed: ${err}` }]));
  // Only the VC Formal command map has fvdisable/fvenable today.
  const toggleCmd = (p: { name: string; enabled: boolean }) => (flavor === "vcf" ? `${p.enabled ? "fvdisable" : "fvenable"} {${p.name}}` : null);

  // Focus after the render that shows the target (the filter only exists in
  // table view; the console is hidden on other tabs).
  useEffect(() => {
    if (!focusReq) return;
    const el = document.getElementById(focusReq.id) as HTMLInputElement | null;
    el?.focus();
    el?.select?.();
  }, [focusReq]);
  const focusFilter = () => {
    setCenter("table");
    setFocusReq((r) => ({ id: "prop-filter", n: (r?.n ?? 0) + 1 }));
  };
  const focusConsole = () => {
    setBottomTab("console");
    setFocusReq((r) => ({ id: "console-input", n: (r?.n ?? 0) + 1 }));
  };
  const toggleSource = () => topFile && setCenter((c) => (c === "source" ? "table" : "source"));
  const DOCK: BottomTab[] = ["console", "details", "messages"];

  // Workbench shortcuts (Help > Keyboard shortcuts lists them). Read through a
  // ref so the listener is installed once but always sees current state.
  const onShortcut = useRef<(e: KeyboardEvent) => void>(() => {});
  onShortcut.current = (e: KeyboardEvent) => {
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "F5" && !mod) proveAll();
    else if (e.key === "F6" && !mod) proveSelected();
    else if (e.key === "F3" && !mod) toggleSource();
    else if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "f") focusFilter();
    else if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "l") focusConsole();
    else if (mod && !e.shiftKey && !e.altKey && ["1", "2", "3"].includes(e.key)) setBottomTab(DOCK[Number(e.key) - 1]);
    // Esc closes an open menu first (the context menu stops its Esc, the
    // menu bar marks it handled) and a field may use it (the filter clears
    // itself); only then does it stop a running prove.
    else if (e.key === "Escape" && running && !e.defaultPrevented) stop();
    else return;
    e.preventDefault();
  };
  useEffect(() => {
    const h = (e: KeyboardEvent) => onShortcut.current(e);
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  const ctxProp = ctx ? allProps.find((p) => p.name === ctx.name) ?? null : null;
  const ctxItems: MenuItem[] = ctxProp
    ? [
        {
          label: "Prove",
          hint: proveOne(ctxProp.name),
          disabled: !idle || ctxProp.kind === "assume",
          title: ctxProp.kind === "assume" ? "Assumptions are not proved" : undefined,
          onClick: () => prove(proveOne(ctxProp.name)),
        },
        { label: "Visualize trace", disabled: true, title: "Coming soon: waveforms for CEX and cover traces" },
        { label: "Show source", disabled: !topFile, onClick: () => { setSelected(ctxProp.name); setCenter("source"); } },
        { sep: true },
        { label: "Copy name", onClick: () => copy(ctxProp.name, ctxProp.name) },
        { label: "Copy expression", onClick: () => copy(ctxProp.expr, `expression of ${ctxProp.name}`) },
        { sep: true },
        (() => {
          const cmd = toggleCmd(ctxProp);
          return {
            label: ctxProp.enabled ? "Disable" : "Enable",
            hint: cmd ?? undefined,
            disabled: !cmd || !idle,
            title: cmd ? undefined : "The JasperGold flavor of the fdv shell has no disable command yet; switch to VC Formal (fvdisable)",
            onClick: () => cmd && run(() => backend.evalTcl(cmd), cmd),
          };
        })(),
      ]
    : [];

  const summary = [
    `${allProps.length} properties`,
    ...(counts.running ? [`${counts.running} running`] : []),
    ...SUMMARY.filter((s) => counts[s]).map((s) => `${counts[s]} ${STATUS_LABEL[s].toLowerCase()}`),
    ...(unprocessed ? [`${unprocessed} unprocessed`] : []),
    ...(assumes ? [`${assumes} assumed`] : []),
  ].join(" · ");

  // Splitters: drag, arrow keys (16px steps), double-click to reset.
  const drag = (e: React.PointerEvent, onMove: (ev: PointerEvent) => void) => {
    e.preventDefault();
    const move = (ev: PointerEvent) => onMove(ev);
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  const startResize = (e: React.PointerEvent) => {
    const y0 = e.clientY;
    const h0 = bottomH;
    drag(e, (ev) => setBottomH(clampBottom(h0 + y0 - ev.clientY, window.innerHeight)));
  };
  const leftRef = useRef<HTMLElement>(null);
  const curLeftW = () => leftW ?? leftRef.current?.getBoundingClientRect().width ?? 236;
  const startLeftResize = (e: React.PointerEvent) => {
    const x0 = e.clientX;
    const w0 = curLeftW();
    drag(e, (ev) => setLeftW(clampLeft(w0 + ev.clientX - x0)));
  };
  const leftKeys = (e: React.KeyboardEvent) => {
    const d = e.key === "ArrowRight" ? 16 : e.key === "ArrowLeft" ? -16 : 0;
    if (!d) return;
    e.preventDefault();
    setLeftW(clampLeft(curLeftW() + d));
  };
  const bottomKeys = (e: React.KeyboardEvent) => {
    const d = e.key === "ArrowUp" ? 16 : e.key === "ArrowDown" ? -16 : 0;
    if (!d) return;
    e.preventDefault();
    setBottomH(clampBottom(bottomH + d, window.innerHeight));
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
    { label: "Edit", items: [{ label: "Find property", keys: "Ctrl+F", onClick: focusFilter }, { label: "Clear filters", onClick: () => { setFilter(""); setStatusFilter(null); } }, { sep: true }, { label: "Console prompt", keys: "Ctrl+L", onClick: focusConsole }] },
    {
      label: "View",
      items: [
        { label: "Task / Design dock", checked: leftOpen, onClick: () => setLeftOpen((o) => !o) },
        { label: "Property table", checked: center === "table", onClick: () => setCenter("table") },
        { label: `Source (${fileName ?? "none"})`, checked: center === "source", keys: "F3", onClick: () => setCenter("source"), disabled: !topFile },
        { sep: true },
        { label: "Console", keys: "Ctrl+1", checked: bottomTab === "console", onClick: () => setBottomTab("console") },
        { label: "Details", keys: "Ctrl+2", checked: bottomTab === "details", onClick: () => setBottomTab("details") },
        { label: "Messages", keys: "Ctrl+3", checked: bottomTab === "messages", onClick: () => setBottomTab("messages") },
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
        { label: "Prove all", hint: proveCmd, keys: "F5", onClick: proveAll, disabled: !idle || !allProps.length },
        { label: "Prove selected", keys: "F6", onClick: proveSelected, disabled: !idle || !current || current.kind === "assume" },
        { label: stopping ? "Stopping…" : "Stop", keys: "Esc", onClick: stop, disabled: !running || stopping },
      ],
    },
    {
      label: "Debug",
      items: [
        { label: "Visualize trace", disabled: true },
        { label: "Show source of selected", keys: "F3", onClick: () => current && setCenter("source"), disabled: !current || !topFile },
      ],
    },
    { label: "Tools", items: [{ label: "Proof profiler (coming soon)", disabled: true }] },
    {
      label: "Help",
      items: [
        {
          label: "Keyboard shortcuts",
          onClick: () => {
            setLog((l) => [...l, ...SHORTCUTS.map(([k, what]) => ({ kind: "out" as const, text: `${k.padEnd(14)}${what}` }))]);
            setBottomTab("console");
          },
        },
        { sep: true },
        { label: "Formal DV v0.1", disabled: true },
      ],
    },
  ];

  return (
    <div
      className="app"
      style={{ "--bottom-h": `${bottomH}px`, ...(leftW ? { "--left-w": `${leftW}px` } : {}) } as React.CSSProperties}
    >
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
          <select value={flavor} disabled={!idle} onChange={(e) => changeFlavor(e.target.value as Flavor)} aria-label="Flavor">
            <option value="jg">JasperGold</option>
            <option value="vcf">VC Formal</option>
          </select>
        </label>
        <span className="vsep" />
        <ToolBtn title={scriptPath ? `source ${scriptPath}` : "Run script (set a script path first)"} onClick={runScript} disabled={!idle}>
          <ScriptIcon />
        </ToolBtn>
        <button className="tool tool-text" onClick={proveAll} disabled={!idle || !allProps.length} title={`${proveCmd} (F5)`}>
          <PlayIcon /> Prove all
        </button>
        <button className="tool tool-text" onClick={stop} disabled={!running || stopping} title="Stop the running prove (Esc)">
          <StopIcon /> {stopping ? "Stopping…" : "Stop"} {!stopping && <span className="kbd">Esc</span>}
        </button>
        <span className="vsep" />
        <ToolBtn title={hasTrace ? "Visualize trace (coming soon)" : "Visualize trace: select a CEX or cover"} disabled>
          <WaveIcon />
        </ToolBtn>
        <ToolBtn title="Source view (F3)" onClick={toggleSource} disabled={!topFile} on={center === "source"}>
          <CodeIcon />
        </ToolBtn>
        <span className="spacer" />
        {running && progress && (
          <span className="progress">
            <span
              className="progress-track"
              role="progressbar"
              aria-label="Prove progress"
              aria-valuemin={0}
              aria-valuemax={progress.total}
              aria-valuenow={progress.done}
            >
              <span className="progress-fill" style={{ width: `${progress.total ? (100 * progress.done) / progress.total : 0}%` }} />
            </span>
            <span className="progress-text mono">
              {progress.done} of {progress.total} · {clock(progress.elapsed_ms)}
            </span>
          </span>
        )}
        {!backend.inTauri && <span className="muted small">Browser preview</span>}
      </div>

      <div className={`workbench ${leftOpen ? "" : "left-closed"}`}>
        {/* ---- left dock ---- */}
        {leftOpen && (
          <aside className="left" ref={leftRef}>
            <div
              className="vsplitter"
              onPointerDown={startLeftResize}
              onDoubleClick={() => setLeftW(null)}
              onKeyDown={leftKeys}
              tabIndex={0}
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize left dock"
              aria-valuemin={LEFT_MIN}
              aria-valuemax={LEFT_MAX}
              aria-valuenow={Math.round(leftW ?? (window.innerWidth <= 1000 ? 200 : 236))}
              title="Drag to resize; double-click to reset"
            />
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
                {(filter.trim() || statusFilter) && (
                  <span className="filter-count" role="status" aria-live="polite">
                    {props.length} of {allProps.length}
                  </span>
                )}
                <input
                  id="prop-filter"
                  className="filter"
                  placeholder="Filter"
                  title="Case-insensitive; matches name, expression or engine (Esc clears)"
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape" && filter) {
                      e.preventDefault();
                      setFilter("");
                    }
                  }}
                  aria-label="Filter properties"
                />
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
              onContext={(p, x, y) => {
                setSelected(p.name);
                setCtx({ name: p.name, x, y });
              }}
            />
          ) : (
            <SourceView text={source} focus={focusLine} />
          )}
        </main>

        {/* ---- bottom dock ---- */}
        <section className="bottom">
          <div
            className="splitter"
            onPointerDown={startResize}
            onDoubleClick={() => setBottomH(BOTTOM_DEFAULT)}
            onKeyDown={bottomKeys}
            tabIndex={0}
            role="separator"
            aria-orientation="horizontal"
            aria-label="Resize bottom dock"
            aria-valuenow={bottomH}
            title="Drag to resize; double-click to reset"
          />
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
          <Console hidden={bottomTab !== "console"} log={log} busy={busy} flavor={flavor} onSubmit={(cmd) => (isProveCmd(cmd) ? prove(cmd) : run(() => backend.evalTcl(cmd), cmd))} />
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
                      <button className="btn" disabled={!idle} onClick={proveSelected}>
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
        {running ? (
          <span className="run">
            <Glyph k="running" />
            {stopping ? "Stopping…" : progress ? `Proving · ${progress.done}/${progress.total}` : "Proving…"}
          </span>
        ) : (
          <span>{busy ? "Working…" : "Idle"}</span>
        )}
      </footer>

      {ctx && ctxProp && <ContextMenu x={ctx.x} y={ctx.y} items={ctxItems} label={`Actions for ${ctxProp.name}`} onClose={closeCtx} />}
    </div>
  );
}
