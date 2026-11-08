// UI v2: a quiet EDA workbench over one fdv shell, laid out the way
// JasperGold / VC Formal users expect: menu bar, tool row, task tree on the
// left, property table in the centre, a bottom dock with the Tcl console,
// property details and messages, and a status bar. Colour is reserved for
// status glyphs; the frame stays neutral. A CEX or cover with a trace opens
// as a closable "Trace: <prop>" document tab (components/TraceView.tsx).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, EngineLine, Flavor, ProveProgress, Reply, SessionView } from "./backend";
import { llmLabel, llmTip, prefill, type InputMode, type LlmProviderEntry, type LlmStatus } from "./ask";
import {
  bound, clock, designScopes, SURFER_MISSING, isProveCmd, matches, parsePorts, STATUS_LABEL, SUMMARY, TASK, sourceFile, sourceLine, statusKey, time, traceOf, withProgress,
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
import { EngineLog } from "./components/EngineLog";
import { SourceView } from "./components/SourceView";
import { TraceView } from "./components/TraceView";
import { Profiler } from "./components/Profiler";
import { CodeIcon, PlayIcon, ScriptIcon, StopIcon, WaveIcon } from "./components/icons";

// Lines the Log tab keeps; older ones are dropped (and counted).
const LOG_CAP = 5000;

const SHORTCUTS: [string, string][] = [
  ["F5", "Prove all"],
  ["F6", "Prove selected"],
  ["F3", "Toggle source view"],
  ["Enter", "Open the trace of a CEX / cover, else its details (table)"],
  ["Left / Right", "Move the trace cursor a cycle (Home / End: first / last)"],
  ["Up / Down", "Move the selection (table)"],
  ["Shift+F10", "Row actions menu (table)"],
  ["Esc", "Close menu; otherwise stop the running prove"],
  ["Ctrl+F", "Filter properties"],
  ["Ctrl+L", "Console prompt"],
  ["Ctrl+1..5", "Console / Details / Messages / Log / Profiler"],
  ["Tab", "Complete a command or property name (console; again to cycle)"],
  ["Ctrl+Shift+A", "Console input mode: Tcl / Ask"],
  ["Enter / Esc", "Run / discard a command Ask suggested (console)"],
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
  // Raw engine output for the Log tab, newest last, capped at LOG_CAP lines.
  const [engineLog, setEngineLog] = useState<{ lines: EngineLine[]; dropped: number }>({ lines: [], dropped: 0 });
  const [autoscroll, setAutoscroll] = useState(true);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>(null);
  // Text of files read for the source view and the design tree, by path.
  const [texts, setTexts] = useState<Record<string, string>>({});
  // The property shown in the trace tab (null: no trace tab), and its VCD.
  const [traceName, setTraceName] = useState<string | null>(null);
  const [trace, setTrace] = useState<{ path: string; text: string | null; error: string | null } | null>(null);
  const { layout, set } = useLayout();
  const { center, left, leftOpen, bottom: bottomTab, bottomH, leftW, profiler } = layout;
  const [setCenter, setLeft, setLeftOpen, setBottomTab, setBottomH, setLeftW, setProfiler] = useMemo(
    () => [set("center"), set("left"), set("leftOpen"), set("bottom"), set("bottomH"), set("leftW"), set("profiler")],
    [set],
  );
  // Tools > Proof profiler (or Ctrl+5) adds the Profiler dock tab and shows
  // it; its × removes it again.
  const openProfiler = useCallback(() => {
    setProfiler(true);
    setBottomTab("profiler");
  }, [setProfiler, setBottomTab]);
  const closeProfiler = useCallback(() => {
    setProfiler(false);
    setBottomTab((t) => (t === "profiler" ? "console" : t));
  }, [setProfiler, setBottomTab]);
  const [theme, setTheme] = useTheme();
  const [focusReq, setFocusReq] = useState<{ id: string; n: number } | null>(null);
  const [ctx, setCtx] = useState<{ name: string; x: number; y: number } | null>(null);
  const closeCtx = useCallback(() => setCtx(null), []);
  const pathRef = useRef<HTMLInputElement>(null);

  // ---- Ask mode ----
  const [inputMode, setInputMode] = useState<InputMode>("tcl");
  const [llm, setLlm] = useState<LlmStatus | null>(null);
  const [llmMenu, setLlmMenu] = useState<{ x: number; y: number; providers: LlmProviderEntry[] } | null>(null);
  const closeLlmMenu = useCallback(() => setLlmMenu(null), []);
  // The answer streaming now (its id and question), and a suggested command
  // waiting on the prompt for Enter / Esc.
  const askRef = useRef<{ id: number; prompt: string; t0: number } | null>(null);
  const [asking, setAsking] = useState(false);
  const [suggestion, setSuggestion] = useState<string | null>(null);
  const askOk = !!llm?.ok;

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
        if (!isProve) {
          setPending((n) => n - 1);
          // A prove run by this call (a sourced script) has ended and its
          // session is in: drop the finished progress so it stops overlaying.
          setProgress((p) => (p && (p.kind === "done" || p.kind === "stopped") ? null : p));
        }
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
    const offEngine = backend.onEngineLog((l) =>
      setEngineLog(({ lines, dropped }) => {
        const all = [...lines, l];
        const cut = Math.max(0, all.length - LOG_CAP);
        return { lines: cut ? all.slice(cut) : all, dropped: dropped + cut };
      }),
    );
    return () => {
      offProgress();
      offConsole();
      offEngine();
    };
  }, []);
  // Ask mode: provider status at start, and the streamed answer.
  const checkLlm = useCallback(() => {
    backend.llmStatus().then(setLlm, (e) => setLlm({ provider: "", model: "", ok: false, error: String(e) }));
  }, []);
  useEffect(checkLlm, [checkLlm]);
  // Offline: Ask is disabled, so leave it.
  useEffect(() => {
    if (llm && !llm.ok) setInputMode("tcl");
  }, [llm]);
  useEffect(() => {
    const offDelta = backend.onAskDelta((d) => {
      if (askRef.current?.id !== d.id) return;
      setLog((log) => {
        const i = log.map((l) => l.kind).lastIndexOf("ask");
        if (i < 0) return log;
        const next = log.slice();
        next[i] = { kind: "ask", text: next[i].text + d.text };
        return next;
      });
    });
    const offDone = backend.onAskDone((d) => {
      const a = askRef.current;
      if (a?.id !== d.id) return;
      askRef.current = null;
      setAsking(false);
      const ms = performance.now() - a.t0;
      const ok = !d.error;
      setLog((log) => {
        // An answer that never streamed (error or empty): say so.
        const i = log.map((l) => l.kind).lastIndexOf("ask");
        const next = i >= 0 && !log[i].text ? [...log.slice(0, i), ...log.slice(i + 1)] : log.slice();
        if (d.error) next.push({ kind: "err", text: `ask: ${d.error}` });
        else if (i >= 0 && !log[i].text) next.push({ kind: "ask", text: d.explanation || "(no answer)" });
        return next;
      });
      setEvents((e) => [...e, { at: new Date(), cmd: `ask ${a.prompt}`, ok, ms, summary: d.error ?? (d.command ? `suggested: ${prefill(d.command)}` : d.explanation.split("\n")[0]) }]);
      if (d.command) setSuggestion(prefill(d.command));
    });
    return () => {
      offDelta();
      offDone();
    };
  }, []);
  const askNow = (prompt: string) => {
    if (askRef.current) return;
    const id = Date.now() * 1000 + Math.floor(Math.random() * 1000);
    askRef.current = { id, prompt, t0: performance.now() };
    setAsking(true);
    setSuggestion(null);
    setLog((l) => [...l, { kind: "ask-in", text: prompt }, { kind: "ask", text: "" }]);
    const context = selected ? `selected property: ${selected}` : undefined;
    backend.ask(id, prompt, context).catch((e) => {
      if (askRef.current?.id !== id) return;
      askRef.current = null;
      setAsking(false);
      setLog((l) => {
        const i = l.map((x) => x.kind).lastIndexOf("ask");
        const next = i >= 0 && !l[i].text ? [...l.slice(0, i), ...l.slice(i + 1)] : l.slice();
        return [...next, { kind: "err", text: `ask: ${e instanceof Error ? e.message : String(e)}` }];
      });
      checkLlm();
    });
  };
  const cancelAsk = () => {
    const a = askRef.current;
    if (a) backend.askCancel(a.id).catch(() => {});
  };
  const setMode = (m: InputMode) => {
    if (m === "ask" && !askOk) return;
    setInputMode(m);
    setSuggestion(null);
  };
  const toggleMode = () => setMode(inputMode === "ask" ? "tcl" : "ask");
  const openLlmMenu = (e: React.MouseEvent<HTMLButtonElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    backend.llmProviders().then(
      (providers) => setLlmMenu({ x: r.left, y: r.top - 2, providers }),
      (err) => setLog((l) => [...l, { kind: "err", text: `LLM providers: ${err}` }]),
    );
  };
  const selectLlm = (name: string) => {
    setLlm(null);
    backend.llmSelect(name).then(setLlm, (e) => setLlm({ provider: name, model: "", ok: false, error: String(e) }));
  };

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
  // Surfer on PATH enables the external-viewer hand-off.
  const [surfer, setSurfer] = useState(false);
  useEffect(() => {
    backend.toolAvailable("surfer").then(setSurfer, () => setSurfer(false));
  }, []);

  const topFile = session?.design.files[0];
  const loadText = useCallback((path: string | undefined) => {
    if (!path) return;
    backend.readText(path).then(
      (t) => setTexts((m) => (m[path] === t ? m : { ...m, [path]: t })),
      () => setTexts((m) => (path in m ? m : { ...m, [path]: "" })),
    );
  }, []);
  useEffect(() => loadText(topFile), [topFile, loadText]);

  // Recomputed every render (cheap): the browser mock mutates the session in
  // place, so memoising on the session reference would go stale.
  // While a prove runs, its progress overlays the session (running rows,
  // k-bound, elapsed); the reply's session takes over when it returns.
  // A script's prove (`source` that adds properties and proves them) keeps
  // its rows from the last progress event until the script's reply lands.
  const allProps = withProgress(session?.props ?? [], live || proving || busy ? progress : null);
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
  // The source view follows the selected property's own file (its script
  // line, or RTL for SVA in the design); the top file otherwise.
  const srcPath = sourceFile(current, topFile);
  useEffect(() => {
    if (srcPath !== topFile) loadText(srcPath);
  }, [srcPath, topFile, loadText]);
  const source = (srcPath && texts[srcPath]) || "";
  const focusLine = sourceLine(current, srcPath, source);
  const fileName = srcPath?.split("/").pop();
  const topText = (topFile && texts[topFile]) || "";
  const ports = useMemo(() => parsePorts(topText), [topText]);
  // slang's scope tree; without it (Yosys front end) the top file's ports.
  const hierarchy = session?.design.hierarchy;
  const scopes = useMemo(() => designScopes(hierarchy, session?.design.top ?? null, ports), [hierarchy, session?.design.top, ports]);
  const currentTrace = traceOf(current);
  const hasTrace = !!currentTrace;
  const traceProp = traceName ? allProps.find((p) => p.name === traceName) ?? null : null;
  const tracePath = traceOf(traceProp);
  const openTrace = (name: string) => {
    setSelected(name);
    setTraceName(name);
    setCenter("trace");
  };
  const closeTrace = () => {
    setTraceName(null);
    setTrace(null);
    setCenter((c) => (c === "trace" ? "table" : c));
  };
  // With the trace tab open, selecting another property that has a trace
  // shows that one; selecting one without keeps the current trace.
  useEffect(() => {
    if (traceName && selected && selected !== traceName && currentTrace) setTraceName(selected);
  }, [selected, traceName, currentTrace]);
  // Load the VCD when the trace tab's path changes. A re-prove clears the
  // path while it runs; the last trace stays on screen until a new one.
  useEffect(() => {
    if (!tracePath) return;
    let live = true;
    setTrace((t) => (t?.path === tracePath ? t : { path: tracePath, text: null, error: null }));
    backend.readTrace(tracePath).then(
      (text) => live && setTrace({ path: tracePath, text, error: null }),
      (e) => live && setTrace({ path: tracePath, text: null, error: String(e) }),
    );
    return () => {
      live = false;
    };
  }, [tracePath]);
  const showTrace = center === "trace" && !!traceProp;
  // The Design tab's double-click: add that signal to the open trace.
  const [addReq, setAddReq] = useState<{ path: string; n: number } | null>(null);
  const note = useCallback((ok: boolean, summary: string) => setEvents((e) => [...e, { at: new Date(), cmd: "add signal", ok, ms: 0, summary }]), []);
  const addSignal = (path: string) => {
    if (!traceName) {
      note(false, `${path}: open a trace first (Visualize trace on a CEX or cover)`);
      setBottomTab("messages");
      return;
    }
    setCenter("trace");
    setAddReq((r) => ({ path, n: (r?.n ?? 0) + 1 }));
  };
  const openInSurfer = (path: string) =>
    backend.openInSurfer(path).then(
      () => note(true, `Opened ${path} in Surfer`),
      (e) => {
        note(false, `Surfer: ${e}`);
        setBottomTab("messages");
      },
    );
  const surferTip = (path: string | null) =>
    !surfer ? SURFER_MISSING : path ? `surfer ${path}` : "Only a CEX or cover result has a trace";
  const onAddResult = useCallback(
    (path: string, r: "added" | "shown" | "missing") =>
      note(r !== "missing", r === "added" ? `Added ${path} to the trace` : r === "shown" ? `${path} is already in the trace` : `${path} is not in this trace`),
    [note],
  );

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
  const DOCK: BottomTab[] = ["console", "details", "messages", "log", "profiler"];

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
    else if (mod && e.shiftKey && !e.altKey && e.key.toLowerCase() === "a") {
      toggleMode();
      focusConsole();
    }
    else if (mod && !e.shiftKey && !e.altKey && e.key === "5") openProfiler();
    else if (mod && !e.shiftKey && !e.altKey && ["1", "2", "3", "4"].includes(e.key)) setBottomTab(DOCK[Number(e.key) - 1]);
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
        {
          label: "Visualize trace",
          hint: traceOf(ctxProp) ?? undefined,
          disabled: !traceOf(ctxProp),
          title: traceOf(ctxProp) ? undefined : "Only a CEX or cover result has a trace",
          onClick: () => openTrace(ctxProp.name),
        },
        {
          label: "Open trace in Surfer",
          disabled: !surfer || !traceOf(ctxProp),
          title: surferTip(traceOf(ctxProp)),
          onClick: () => {
            const t = traceOf(ctxProp);
            if (t) openInSurfer(t);
          },
        },
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
        { label: "Log", keys: "Ctrl+4", checked: bottomTab === "log", onClick: () => setBottomTab("log") },
        { label: "Profiler", keys: "Ctrl+5", checked: profiler && bottomTab === "profiler", onClick: openProfiler },
        { sep: true },
        { label: "Input mode: Tcl", checked: inputMode === "tcl", onClick: () => setMode("tcl") },
        {
          label: "Input mode: Ask",
          keys: "Ctrl+Shift+A",
          checked: inputMode === "ask",
          disabled: !askOk,
          title: askOk ? undefined : llmTip(llm),
          onClick: () => {
            setMode("ask");
            focusConsole();
          },
        },
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
        { label: "Visualize trace", disabled: !hasTrace, onClick: () => current && hasTrace && openTrace(current.name) },
        { label: "Show source of selected", keys: "F3", onClick: () => current && setCenter("source"), disabled: !current || !topFile },
      ],
    },
    {
      label: "Tools",
      items: [{ label: "Proof profiler", keys: "Ctrl+5", checked: profiler, onClick: () => (profiler ? closeProfiler() : openProfiler()) }],
    },
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

  const centerTabs = (
    <Tabs
      tabs={[
        ["table", "Property Table"],
        ...(topFile ? ([["source", fileName ?? "Source"]] as [string, string][]) : []),
        ...(traceName ? ([["trace", `Trace: ${traceName}`, closeTrace]] as [string, string, () => void][]) : []),
      ]}
      value={center === "trace" && !traceName ? "table" : center}
      onChange={(t) => setCenter(t as Center)}
    />
  );

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
          <StopIcon /> {stopping ? "Stopping…" : "Stop"}
        </button>
        <span className="vsep" />
        <ToolBtn
          title={hasTrace ? `Visualize trace (${currentTrace})` : "Visualize trace: select a CEX or cover"}
          disabled={!hasTrace}
          on={showTrace && traceName === current?.name}
          onClick={() => current && openTrace(current.name)}
        >
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
                  onOpen={(n) => (traceOf(allProps.find((p) => p.name === n) ?? null) ? openTrace(n) : select(n))}
                />
              ) : (
                <DesignTree scopes={scopes} files={session?.design.files ?? []} fallback={!hierarchy?.length} onAddSignal={addSignal} />
              )}
            </div>
          </aside>
        )}

        {/* ---- centre ---- */}
        <main className="center">
          {showTrace ? (
            <TraceView
              tabs={centerTabs}
              prop={traceProp}
              path={trace?.path ?? tracePath ?? ""}
              text={trace?.text ?? null}
              error={trace?.error}
              addRequest={addReq}
              onAddResult={onAddResult}
              actions={
                // The title sits on a wrapper too: a disabled button shows none.
                <span className="surfer-wrap" title={surferTip(tracePath)}>
                  <button className="btn" disabled={!surfer || !tracePath} onClick={() => tracePath && openInSurfer(tracePath)} title={surferTip(tracePath)}>
                    Open in Surfer
                  </button>
                </span>
              }
            />
          ) : (
          <>
          <div className="strip">
            {centerTabs}
            {center !== "source" ? (
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
              <span className="strip-summary mono">{srcPath}{focusLine ? `:${focusLine}` : ""}</span>
            )}
          </div>
          {center !== "source" ? (
            <PropTable
              props={props}
              total={allProps.length}
              selected={selected}
              onSelect={select}
              onOpen={(p) => (traceOf(p) ? openTrace(p.name) : select(p.name))}
              onContext={(p, x, y) => {
                setSelected(p.name);
                setCtx({ name: p.name, x, y });
              }}
            />
          ) : (
            <SourceView text={source} focus={focusLine} />
          )}
          </>
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
              ["log", "Log"],
              ...(profiler ? ([["profiler", "Profiler", closeProfiler]] as [string, string, () => void][]) : []),
            ]}
            value={bottomTab}
            onChange={(t) => setBottomTab(t as BottomTab)}
            extra={
              <div className="mode-switch" role="group" aria-label="Console input mode">
                <button aria-pressed={inputMode === "tcl"} title="Tcl commands (Ctrl+Shift+A toggles)" onClick={() => { setMode("tcl"); focusConsole(); }}>
                  Tcl
                </button>
                <button
                  aria-pressed={inputMode === "ask"}
                  disabled={!askOk}
                  title={askOk ? `Ask ${llm?.provider}/${llm?.model} in plain words (Ctrl+Shift+A toggles)` : llmTip(llm)}
                  onClick={() => { setMode("ask"); focusConsole(); }}
                >
                  Ask
                </button>
              </div>
            }
          />
          {/* kept mounted so history and scroll survive tab switches */}
          <Console
            hidden={bottomTab !== "console"}
            log={log}
            busy={busy}
            flavor={flavor}
            complete={backend.complete}
            onSubmit={(cmd) => (isProveCmd(cmd) ? prove(cmd) : run(() => backend.evalTcl(cmd), cmd))}
            mode={inputMode}
            asking={asking}
            suggestion={suggestion}
            onAsk={askNow}
            onCancelAsk={cancelAsk}
            onSuggestion={() => setSuggestion(null)}
          />
          <EngineLog
            hidden={bottomTab !== "log"}
            lines={engineLog.lines}
            dropped={engineLog.dropped}
            autoscroll={autoscroll}
            onAutoscroll={setAutoscroll}
            onClear={() => setEngineLog({ lines: [], dropped: 0 })}
          />
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
                    <dt>Trace</dt>
                    <dd>
                      {currentTrace ? (
                        <button className="link mono" onClick={() => openTrace(current.name)} title="Open in the trace tab">
                          Trace: {currentTrace}
                          {current.status.depth != null ? ` · ${current.status.depth + 1} cycles` : ""}
                        </button>
                      ) : (
                        "—"
                      )}
                    </dd>
                    <dt>Source</dt>
                    <dd>
                      {fileName ? (
                        <button className="link mono" onClick={() => setCenter("source")} title={current.source ? `${srcPath}:${focusLine}` : "Not recorded; best guess in the top file"}>
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
                    <button
                      className="btn"
                      disabled={!hasTrace}
                      title={hasTrace ? `Open ${currentTrace}` : "No trace for this result"}
                      onClick={() => openTrace(current.name)}
                    >
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
          {bottomTab === "profiler" && profiler && <Profiler prop={current} props={allProps} />}
        </section>
      </div>

      {/* ---- status bar ---- */}
      <footer className="statusbar">
        <span>top: <span className="mono">{session?.design.top ?? "—"}</span></span>
        <span>clock: <span className="mono">{session?.clocks.map((c) => c.signal).join(", ") || "—"}</span></span>
        <span>reset: <span className="mono">{session?.reset ?? "—"}</span></span>
        <span>{flavor === "jg" ? "JasperGold" : "VC Formal"} flavor</span>
        <span className="spacer" />
        <button className="statusbar-btn" title={llmTip(llm)} aria-haspopup="menu" onClick={openLlmMenu}>
          {llmLabel(llm)}
        </button>
        {running ? (
          <span className="run">
            <Glyph k="running" />
            {stopping ? "Stopping…" : progress ? `Proving · ${progress.done}/${progress.total}` : "Proving…"}
          </span>
        ) : (
          <span>{busy ? "Working…" : "Idle"}</span>
        )}
      </footer>

      {llmMenu && (
        <ContextMenu
          x={llmMenu.x}
          y={llmMenu.y}
          above
          label="LLM provider"
          onClose={closeLlmMenu}
          items={[
            ...llmMenu.providers.map((p) => ({
              label: p.name,
              hint: p.model,
              checked: p.selected,
              title: p.base_url || p.kind,
              onClick: () => selectLlm(p.name),
            })),
            { sep: true as const },
            { label: "Check connection", onClick: checkLlm },
          ]}
        />
      )}
      {ctx && ctxProp && <ContextMenu x={ctx.x} y={ctx.y} items={ctxItems} label={`Actions for ${ctxProp.name}`} onClose={closeCtx} />}
    </div>
  );
}
