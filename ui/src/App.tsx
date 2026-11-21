// UI v2: a quiet EDA workbench over one informal shell, laid out the way
// JasperGold / VC Formal users expect: menu bar, tool row, task tree on the
// left, property table in the centre, a bottom dock with the Tcl console,
// property details and messages, and a status bar. Colour is reserved for
// status glyphs; the frame stays neutral. A CEX or cover with a trace opens
// as a closable "Trace: <prop>" document tab (components/TraceView.tsx).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { backend, type AiStatus, type DesignInfo as DesignInfoData, type EffortLevel, EngineLine, Flavor, type FormalLintFindingView, type FormalQuickFixView, type GuiRequest, type GuiState, type OrchestrationMode, ProveProgress, Reply, type SchematicGraphView, SessionView, type WhyCexReport } from "./backend";
import { llmLabel, llmTip, prefill, tclWord, type InputMode, type LlmProviderEntry, type LlmStatus } from "./ask";
import {
  bound, clock, designScopes, memText, OPTIONAL_COLS, SURFER_MISSING, isProveCmd, parsePorts, STATUS_LABEL, sourceFile, sourceLine, taskOf, time, traceOf, withProgress,
  type BottomTab, type Center, type ColKey, type LeftTab, type LogEvent, type LogLine, type StatusFilter, type ViewStatus,
} from "./model";
import { useWorkbenchStore, type LayoutPreset, type WalkthroughStep } from "./store/workbenchStore";
import {
  activeFilterChips,
  findNewlyHiddenProved,
  guidedNextAction,
  resolveProveAll,
  selectAndReveal,
  summarySegments,
  taskEnvSummary,
  visibleProps,
  type HiddenVerdictNotice,
} from "./store/workbenchController";
import { resolveSvgPalette } from "./svgTheme";
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
import { SchematicView } from "./components/SchematicView";
import { Profiler } from "./components/Profiler";
import { DesignInfo } from "./components/DesignInfo";
import { KIND_LABEL, Proposals } from "./components/Proposals";
import { FormalApps, type AppMode } from "./components/FormalApps";
import { CommandPalette, type PaletteCommand } from "./components/CommandPalette";
import { WalkthroughBar } from "./components/WalkthroughBar";
import { WelcomePane } from "./components/WelcomePane";
import { WorkbenchDock } from "./components/WorkbenchDock";
import { CodeIcon, PlayIcon, SchematicIcon, ScriptIcon, StopIcon, WaveIcon } from "./components/icons";

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
  ["Ctrl+K / Cmd+K", "Universal command palette"],
  ["Ctrl+Shift+A", "Console input mode: Tcl / Ask"],
  ["Enter / Esc", "Run / discard a command Ask suggested (console)"],
];

const POPOUT_KIND: "trace" | "schematic" | null = (() => {
  if (typeof window === "undefined") return null;
  const p = new URLSearchParams(window.location.search).get("popout");
  return p === "trace" || p === "schematic" ? p : null;
})();

const CEX_BOTTOM_DEFAULT = 148;
const IS_MAC_OVERLAY =
  backend.inTauri && typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || navigator.userAgent);

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
  // The task the table shows (the task tree's selection); null: every task.
  const [taskFilter, setTaskFilter] = useState<string | null>(null);
  const [hiddenNotice, setHiddenNotice] = useState<HiddenVerdictNotice | null>(null);
  const [hintDismissed, setHintDismissed] = useState(false);
  // Text of files read for the source view and the design tree, by path.
  const [texts, setTexts] = useState<Record<string, string>>({});
  // The property shown in the trace tab (null: no trace tab), its binary IFTR stream, and raw VCD text.
  const [traceName, setTraceName] = useState<string | null>(null);
  const [trace, setTrace] = useState<{ path: string; text: string | null; binary: Uint8Array | null; error: string | null } | null>(null);
  const traceSvgCaptureRef = useRef<(() => string | null) | null>(null);
  const schemSvgCaptureRef = useRef<(() => string | null) | null>(null);

  // Fine-grained Zustand slices
  const setStoreProgress = useWorkbenchStore((st) => st.setProveProgress);
  const layoutPreset = useWorkbenchStore((st) => st.layoutPreset);
  const setLayoutPreset = useWorkbenchStore((st) => st.setLayoutPreset);
  const commandPaletteOpen = useWorkbenchStore((st) => st.commandPaletteOpen);
  const setCommandPaletteOpen = useWorkbenchStore((st) => st.setCommandPaletteOpen);
  const schematicProp = useWorkbenchStore((st) => st.schematicProp);
  const schematicDepth = useWorkbenchStore((st) => st.schematicDepth);
  const setSchematicTarget = useWorkbenchStore((st) => st.setSchematicTarget);
  const traceAnnotations = useWorkbenchStore((st) => st.traceAnnotations);
  const setTraceAnnotations = useWorkbenchStore((st) => st.setTraceAnnotations);
  const sliceOnlyControlled = useWorkbenchStore((st) => st.sliceOnlyControlled);
  const setSliceOnlyControlled = useWorkbenchStore((st) => st.setSliceOnlyControlled);
  const walkthrough = useWorkbenchStore((st) => st.walkthrough);
  const setWalkthrough = useWorkbenchStore((st) => st.setWalkthrough);
  const followAgent = useWorkbenchStore((st) => st.followAgent);
  const setFollowAgent = useWorkbenchStore((st) => st.setFollowAgent);
  const markAgentDriven = useWorkbenchStore((st) => st.markAgentDriven);
  const focusChips = useWorkbenchStore((st) => st.focusChips);
  const addFocusChip = useWorkbenchStore((st) => st.addFocusChip);
  const removeFocusChip = useWorkbenchStore((st) => st.removeFocusChip);
  const clearFocusChips = useWorkbenchStore((st) => st.clearFocusChips);
  const lastCapture = useWorkbenchStore((st) => st.lastCapture);
  const setLastCapture = useWorkbenchStore((st) => st.setLastCapture);
  const recentSessions = useWorkbenchStore((st) => st.recentSessions);
  const recordRecentSession = useWorkbenchStore((st) => st.recordRecentSession);

  const [schematicGraph, setSchematicGraph] = useState<SchematicGraphView | null>(null);
  const [schematicLoading, setSchematicLoading] = useState(false);
  const [schematicError, setSchematicError] = useState<string | null>(null);
  const [lintFindings, setLintFindings] = useState<FormalLintFindingView[]>([]);
  const { layout, set } = useLayout();
  const { center, left, leftOpen, bottom: bottomTab, bottomH, leftW, profiler, hidden } = layout;
  const [setCenter, setLeft, setLeftOpen, setBottomTab, setBottomH, setLeftW, setProfiler] = useMemo(
    () => [set("center"), set("left"), set("leftOpen"), set("bottom"), set("bottomH"), set("leftW"), set("profiler")],
    [set],
  );
  const [cexBottomH, setCexBottomH] = useState(CEX_BOTTOM_DEFAULT);
  const activeBottomH = layoutPreset === "cex-debug" ? cexBottomH : bottomH;
  const setActiveBottomH = useCallback(
    (h: number) => {
      if (layoutPreset === "cex-debug") setCexBottomH(h);
      else setBottomH(h);
    },
    [layoutPreset, setBottomH],
  );
  const [narrow, setNarrow] = useState(() => (typeof window !== "undefined" ? window.innerWidth < 720 : false));
  const [drawerOpen, setDrawerOpen] = useState(false);
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia?.("(max-width: 719.98px)");
    const sync = () => {
      const isNarrow = mq ? mq.matches : window.innerWidth < 720;
      setNarrow(isNarrow);
      if (!isNarrow) setDrawerOpen(false);
    };
    sync();
    mq?.addEventListener?.("change", sync);
    window.addEventListener("resize", sync);
    return () => {
      mq?.removeEventListener?.("change", sync);
      window.removeEventListener("resize", sync);
    };
  }, []);
  const showLeft = narrow ? drawerOpen : leftOpen;
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
  const [appsOpen, setAppsOpen] = useState(false);
  const [appMode, setAppMode] = useState<AppMode>("cov");
  const openApps = useCallback((mode?: AppMode) => {
    if (mode) setAppMode(mode);
    setAppsOpen(true);
    setBottomTab("apps");
  }, [setBottomTab]);
  const closeApps = useCallback(() => {
    setAppsOpen(false);
    setBottomTab((t) => (t === "apps" ? "console" : t));
  }, [setBottomTab]);
  const [whyCexMap, setWhyCexMap] = useState<Record<string, WhyCexReport>>({});
  const [whyCexOpen, setWhyCexOpen] = useState(false);
  const [whyCexLoading, setWhyCexLoading] = useState(false);
  const toggleWhyCex = useCallback(async () => {
    if (!traceName) return;
    if (whyCexOpen) {
      setWhyCexOpen(false);
      return;
    }
    setWhyCexOpen(true);
    if (!whyCexMap[traceName]) {
      setWhyCexLoading(true);
      try {
        const rep = await backend.whyCex(traceName);
        setWhyCexMap((m) => ({ ...m, [traceName]: rep }));
      } catch (e) {
        setLog((l) => [...l, { kind: "err", text: `why_cex: ${String(e)}` }]);
        setWhyCexOpen(false);
      } finally {
        setWhyCexLoading(false);
      }
    }
  }, [traceName, whyCexOpen, whyCexMap]);
  const [theme, setTheme] = useTheme();
  const [focusReq, setFocusReq] = useState<{ id: string; n: number } | null>(null);
  const [ctx, setCtx] = useState<{ name: string; x: number; y: number } | null>(null);
  const closeCtx = useCallback(() => setCtx(null), []);
  // The column chooser (header right-click, the strip's Columns button).
  const [colMenu, setColMenu] = useState<{ x: number; y: number } | null>(null);
  const closeColMenu = useCallback(() => setColMenu(null), []);
  const setHidden = useMemo(() => set("hidden"), [set]);
  const toggleCol = (k: ColKey) => setHidden((h) => (h.includes(k) ? h.filter((x) => x !== k) : [...h, k]));
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
        return r;
      } catch (e) {
        setLog((l) => [...l, { kind: "err", text: String(e) }]);
        note(false, String(e));
        return null;
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
      setStoreProgress(p);
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
    // A lookup goes above the answer it leads to.
    const offStep = backend.onAskStep((d) => {
      if (askRef.current?.id !== d.id) return;
      setLog((log) => {
        const i = log.map((l) => l.kind).lastIndexOf("ask");
        if (i < 0) return log;
        return [...log.slice(0, i), { kind: "ask-step", text: d.text }, ...log.slice(i)];
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
      offStep();
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
    const chipSummary = focusChips.map((c) => c.detail).join(" | ");
    const baseCtx = selected ? `selected property: ${selected}` : "";
    const context = [baseCtx, chipSummary].filter(Boolean).join("; ") || undefined;
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
    const beforeProps = (session?.props ?? []).map((p) => ({ ...p, status: { ...p.status } }));
    provingRef.current = true;
    setProving(true);
    setProgress(null);
    setHiddenNotice(null);
    try {
      const r = await run(() => backend.evalTcl(cmd), cmd, true);
      if (r?.session?.props) {
        const availTasks = r.session.tasks?.map((t) => t.name);
        const notice = findNewlyHiddenProved(
          beforeProps,
          r.session.props,
          { statusFilter, taskFilter, filter },
          availTasks,
        );
        setHiddenNotice(notice);
      }
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
    // A script given on the command line may have started the shell in VC Formal flavor.
    backend.flavor().then(setFlavor, () => {});
  }, [run]);
  // The AI interface (docs/ai-interface.md): an agent on the app's socket
  // works on this session. After each of its calls the backend says so,
  // and the session and the proposals are read again.
  const [ai, setAi] = useState<AiStatus | null>(null);
  const [deciding, setDeciding] = useState(false);
  const refreshAi = useCallback(() => backend.aiStatus().then(setAi, () => {}), []);
  useEffect(() => {
    refreshAi();
    return backend.onAiActivity(() => {
      refreshAi();
      run(() => backend.snapshot());
    });
  }, [refreshAi, run]);
  const waiting = ai?.proposals.filter((p) => p.state === "pending").length ?? 0;
  // The person's answer to a proposal. An accepted one is in the session
  // when this returns; both answers are noted in the console and Messages.
  const decide = useCallback(
    (id: number, accept: boolean) => {
      const p = ai?.proposals.find((q) => q.id === id);
      const what = p ? `${KIND_LABEL[p.kind].toLowerCase()}${p.name ? ` ${p.name}` : ""}` : `proposal ${id}`;
      const cmd = `${accept ? "accept" : "reject"} AI proposal: ${what}`;
      const t0 = performance.now();
      const done = (ok: boolean, summary: string) => {
        setLog((l) => [...l, { kind: ok ? "out" : "err", text: `${cmd}${summary ? `: ${summary}` : ""}` }]);
        setEvents((e) => [...e, { at: new Date(), cmd, ok, ms: performance.now() - t0, summary }]);
      };
      setDeciding(true);
      backend
        .aiDecide(id, accept)
        .then(
          (r) => done(r.state !== "failed", r.error ?? ""),
          (e) => done(false, String(e)),
        )
        .finally(() => {
          setDeciding(false);
          refreshAi();
          run(() => backend.snapshot());
        });
    },
    [ai, refreshAi, run],
  );
  // The Proposals tab exists while there is something to show in it.
  useEffect(() => {
    if (ai && !ai.proposals.length) setBottomTab((t) => (t === "proposals" ? "console" : t));
  }, [ai, setBottomTab]);
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
  const tasks = session?.tasks?.map((t) => t.name);
  // A task that is gone (task -delete) no longer filters.
  const shownTask = taskFilter && (tasks ?? []).includes(taskFilter) ? taskFilter : null;
  const props = visibleProps(allProps, { filter, statusFilter, taskFilter }, tasks);
  const counts: Partial<Record<ViewStatus, number>> = {};
  for (const p of allProps) if (p.kind !== "assume") counts[p.status.status] = (counts[p.status.status] ?? 0) + 1;
  const assumes = allProps.filter((p) => p.kind === "assume").length;

  const current = allProps.find((p) => p.name === selected) ?? null;
  const provePlan = resolveProveAll(allProps, session?.current_task, shownTask, flavor);
  const proveCmd = provePlan.cmd ?? (flavor === "jg" ? "prove -all" : "check_fv");
  const filterChips = activeFilterChips({ statusFilter, taskFilter, filter }, tasks);
  const summaryPills = summarySegments(allProps);
  const envSummary = taskEnvSummary(session, allProps, shownTask);
  const guidedHint = guidedNextAction(allProps, provePlan, running, busy);
  // UX-2: Selecting a property in Tasks (or a verdict notice) reveals it in PropTable.
  const selectAndRevealProp = useCallback(
    (name: string) => {
      const next = selectAndReveal(
        { selected, statusFilter, taskFilter, filter },
        name,
        allProps,
        tasks,
      );
      setSelected(next.selected);
      setStatusFilter(next.statusFilter);
      setTaskFilter(next.taskFilter);
      setFilter(next.filter);
      setHiddenNotice(null);
    },
    [selected, statusFilter, taskFilter, filter, allProps, tasks],
  );
  // Braces keep Tcl from reading `gen[0].p` as a command substitution.
  const proveOne = (n: string) => (flavor === "jg" ? `prove -property ${tclWord(n)}` : `check_fv -property {${n}}`);
  // The source view follows the selected property's own file (its script
  // line, or RTL for SVA in the design); the top file otherwise.
  // In CEX Debug, Tcl-defined properties auto-pivot to the primary RTL `.sv`
  // file so live `@C<k>` values and `SV-FPV` lint appear immediately, with
  // file-switcher pills (`top.sv` · `script.tcl`) to flip back anytime.
  // Unless an agent pointed at other lines (gui_highlight_source): those
  // stay until the person selects a property.
  const [srcShow, setSrcShow] = useState<{ file: string; first: number; last: number } | null>(null);
  const [srcFileOverride, setSrcFileOverride] = useState<string | null>(null);
  useEffect(() => {
    setSrcShow(null);
    setSrcFileOverride(null);
  }, [selected]);
  useEffect(() => {
    setSrcFileOverride(null);
  }, [layoutPreset]);
  const propSrcFile = current?.source?.file ?? null;
  const rtlFiles = useMemo(
    () => (session?.design.files ?? []).filter((f) => /\.(sv|svh|v)$/i.test(f)),
    [session?.design.files],
  );
  const primaryRtlFile = rtlFiles[0] ?? topFile;
  const autoPivotToRtl =
    layoutPreset === "cex-debug" &&
    !srcShow &&
    !srcFileOverride &&
    Boolean(propSrcFile?.endsWith(".tcl")) &&
    Boolean(primaryRtlFile);
  const srcPath =
    srcShow?.file ??
    srcFileOverride ??
    (autoPivotToRtl ? primaryRtlFile : sourceFile(current, topFile));
  const sourceSwitchFiles = useMemo(() => {
    const list: string[] = [];
    for (const f of rtlFiles) {
      if (f && !list.includes(f)) list.push(f);
    }
    if (propSrcFile && !list.includes(propSrcFile)) list.push(propSrcFile);
    if (srcPath && !list.includes(srcPath)) list.push(srcPath);
    return list.length > 1 ? list : [];
  }, [rtlFiles, propSrcFile, srcPath]);
  useEffect(() => {
    if (srcPath) loadText(srcPath);
  }, [srcPath, loadText]);
  const source = (srcPath && texts[srcPath]) || "";
  const focusLine = srcShow ? srcShow.first : sourceLine(current, srcPath, source);
  const focusEnd = srcShow?.last;
  const fileName = srcPath?.split("/").pop();
  const topText = (topFile && texts[topFile]) || "";
  const ports = useMemo(() => parsePorts(topText), [topText]);
  // The front end's scope tree; without one, the top file's ports.
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
  // Load both binary IFTR stream (zero-JSON Uint8Array IPC) and VCD text when the trace tab's path changes.
  useEffect(() => {
    if (!tracePath) return;
    let live = true;
    setTrace((t) => (t?.path === tracePath ? t : { path: tracePath, text: null, binary: null, error: null }));
    Promise.all([
      backend.readTraceBin(tracePath).catch(() => null),
      backend.readTrace(tracePath),
    ]).then(
      ([binary, text]) => live && setTrace({ path: tracePath, text, binary, error: null }),
      (e) => live && setTrace({ path: tracePath, text: null, binary: null, error: String(e) }),
    );
    return () => {
      live = false;
    };
  }, [tracePath]);
  const showTrace = center === "trace" && !!traceProp;

  // Live formal-ready lint (`svlang lint` SV-FPV-001..005) when source view opens on an RTL file
  const refreshLint = useCallback(
    (targetFile?: string) => {
      if (!session?.elaborated) {
        setLintFindings([]);
        return;
      }
      backend
        .formalLint(targetFile)
        .then((list) => setLintFindings(list))
        .catch(() => setLintFindings([]));
    },
    [session?.elaborated],
  );
  useEffect(() => {
    if ((center === "source" || layoutPreset !== "triage") && srcPath && /\.(sv|svh|v)$/i.test(srcPath)) {
      refreshLint(srcPath);
    }
  }, [center, layoutPreset, srcPath, refreshLint]);

  const applyQuickFix = useCallback(
    (fix: FormalQuickFixView) => {
      backend
        .applyFormalFix(fix)
        .then((res) => {
          if (res.updatedText !== undefined) {
            setTexts((m) => ({ ...m, [fix.file]: res.updatedText! }));
          } else {
            loadText(fix.file);
          }
          refreshLint(fix.file);
          setEvents((ev) => [
            ...ev,
            { at: new Date(), cmd: `quick-fix ${fix.file.split("/").pop()}:${fix.line}`, ok: true, ms: 0, summary: fix.description },
          ]);
        })
        .catch((e) => setLog((l) => [...l, { kind: "err", text: `quick-fix: ${String(e)}` }]));
    },
    [loadText, refreshLint],
  );

  // Interactive COI & Causal Schematic loader
  const activeSchemProp = useMemo(() => {
    if (schematicProp && allProps.some((p) => p.name === schematicProp)) return schematicProp;
    if (current && current.kind !== "assume") return current.name;
    return allProps.find((p) => p.kind !== "assume")?.name ?? null;
  }, [schematicProp, allProps, current]);

  useEffect(() => {
    const needSchem = center === "schematic" || layoutPreset === "cex-debug" || layoutPreset === "split" || POPOUT_KIND === "schematic";
    if (!needSchem || !activeSchemProp || !session?.elaborated) return;
    let alive = true;
    setSchematicLoading(true);
    setSchematicError(null);
    backend
      .schematic(activeSchemProp, schematicDepth)
      .then((g) => {
        if (alive) setSchematicGraph(g);
      })
      .catch((e) => {
        if (alive) setSchematicError(String(e instanceof Error ? e.message : e));
      })
      .finally(() => {
        if (alive) setSchematicLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [center, layoutPreset, activeSchemProp, schematicDepth, session?.elaborated]);

  const openSchematic = useCallback(
    (propName?: string, depth?: number) => {
      const target = propName ?? activeSchemProp;
      if (target) {
        setSelected(target);
        setSchematicTarget(target, depth);
      }
      setCenter("schematic");
    },
    [activeSchemProp, setSchematicTarget, setCenter],
  );

  // MCP App (`ui://informal/workbench.html`) live tool result updates
  useEffect(() => {
    if (!backend.inMcpApp) return;
    return backend.onMcpToolResult((sc) => {
      const schema = String(sc.schema ?? "");
      if (schema === "informal.workbench/1" && sc.session) {
        setSession(sc.session as SessionView);
        if (typeof sc.focus_property === "string") setSelected(sc.focus_property);
        if (typeof sc.view === "string" && ["table", "source", "trace", "schematic"].includes(sc.view)) {
          setCenter(sc.view as Center);
        }
      } else if (schema === "informal.why_cex/1" && typeof sc.property === "string") {
        setWhyCexMap((m) => ({ ...m, [sc.property as string]: sc as unknown as WhyCexReport }));
        setWhyCexOpen(true);
      } else if (schema === "informal.cex_diagnose/1" && sc.property && typeof (sc.property as { name?: string }).name === "string") {
        const propName = (sc.property as { name: string }).name;
        const slice = sc.causal_slice as Record<string, unknown> | undefined;
        setSelected(propName);
        setTraceName(propName);
        if (slice) {
          setWhyCexMap((m) => ({
            ...m,
            [propName]: {
              property: propName,
              status: String((sc.property as { status?: string }).status ?? "cex"),
              failure_frame: Number(slice.failure_frame ?? 0),
              total_frames: Number(slice.total_frames ?? 0),
              static_cone_signals: Number(slice.static_cone_signals ?? 0),
              active_slice_signals: Number(slice.active_slice_signals ?? 0),
              active_signals: Array.isArray(slice.active_signals) ? (slice.active_signals as string[]) : [],
              events: Array.isArray(slice.events) ? (slice.events as WhyCexReport["events"]) : [],
            },
          }));
          setWhyCexOpen(true);
        }
      } else if (schema === "informal.schematic/1") {
        setSchematicGraph(sc as unknown as SchematicGraphView);
        setCenter("schematic");
      } else {
        void backend.snapshot().then((r) => r.session && setSession(r.session));
      }
    });
  }, [setCenter]);

  // Dynamic standalone window title
  useEffect(() => {
    if (typeof document === "undefined") return;
    const top = session?.design.top;
    if (!top) {
      document.title = "InFormal";
      return;
    }
    const badges = [
      counts.proven ? `${counts.proven} proven` : "",
      counts.cex ? `${counts.cex} CEX` : "",
      counts.covered ? `${counts.covered} covered` : "",
    ].filter(Boolean);
    document.title = badges.length ? `InFormal — ${top} [${badges.join(" · ")}]` : `InFormal — ${top}`;
  }, [session?.design.top, counts.proven, counts.cex, counts.covered]);

  // Reverse UI -> AI `@context` pin helper
  const pinContext = useCallback(
    (label: string, detail: string) => {
      addFocusChip({ label, detail });
      if (backend.inMcpApp) {
        const allText = [...focusChips.map((c) => c.detail), detail].join(" | ");
        void backend.attachHostContext(allText);
      } else if (askOk) {
        setInputMode("ask");
        setBottomTab("console");
        setFocusReq((r) => ({ id: "console-input", n: (r?.n ?? 0) + 1 }));
      }
    },
    [addFocusChip, focusChips, askOk, setBottomTab],
  );

  // Synchronize a walkthrough step across Trace + Callouts + Source
  const applyWalkthroughStep = useCallback(
    (st: WalkthroughStep) => {
      markAgentDriven();
      if (st.property) {
        const p = allProps.find((q) => q.name === st.property);
        if (p && traceOf(p)) {
          setSelected(st.property);
          setTraceName(st.property);
          if (layoutPreset === "triage" && !st.file) setCenter("trace");
        }
      }
      if (st.frame != null) {
        setCursorReq((r) => ({ cycle: st.frame!, n: (r?.n ?? 0) + 1 }));
      }
      if (st.signals?.length && st.frame != null) {
        setTraceAnnotations(
          st.signals.map((sig) => ({
            frame: st.frame!,
            signal: sig,
            label: st.label,
            tone: "danger" as const,
          })),
        );
      }
      if (st.file) {
        backend.readText(st.file).then(
          (t) => {
            setTexts((m) => ({ ...m, [st.file!]: t }));
            const first = st.first_line ?? 1;
            setSrcShow({ file: st.file!, first, last: st.last_line ?? first });
            if (layoutPreset === "triage" && st.frame == null) setCenter("source");
          },
          () => {},
        );
      }
    },
    [allProps, layoutPreset, markAgentDriven, setCenter, setTraceAnnotations],
  );

  // 1-Click AI Co-Pilot CEX Root-Cause Walkthrough builder
  const launchCexWalkthrough = useCallback(
    async (propName: string) => {
      openTrace(propName);
      setLayoutPreset("cex-debug");
      let rep = whyCexMap[propName];
      if (!rep) {
        setWhyCexLoading(true);
        try {
          rep = await backend.whyCex(propName);
          setWhyCexMap((m) => ({ ...m, [propName]: rep! }));
        } catch (e) {
          setLog((l) => [...l, { kind: "err", text: `why_cex: ${String(e)}` }]);
          return;
        } finally {
          setWhyCexLoading(false);
        }
      }
      setWhyCexOpen(true);
      const steps: WalkthroughStep[] = rep.events.map((ev, idx) => ({
        label: idx === rep.events.length - 1 ? `Failure @C${ev.frame}` : `Step ${idx + 1} @C${ev.frame}`,
        note: `${ev.signal} = ${ev.prev_value ? `${ev.prev_value} → ${ev.value}` : ev.value} (${ev.reason.replace(/_/g, " ")})`,
        property: propName,
        frame: ev.frame,
        signals: [ev.signal],
        file: topFile,
      }));
      if (steps.length > 0) {
        setFollowAgent(true);
        setWalkthrough({ title: `Why ${propName} fails`, steps, step: 0 });
        applyWalkthroughStep(steps[0]);
      }
    },
    [whyCexMap, topFile, setLayoutPreset, setFollowAgent, setWalkthrough, applyWalkthroughStep],
  );
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
  // ---- An agent's window tools (docs/ai-interface.md) ----
  // What the window shows, reported after every change for gui_state.
  const [traceView, setTraceView] = useState<{ cursor: number; signals: string[] } | null>(null);
  const [cursorReq, setCursorReq] = useState<{ cycle: number; n: number } | null>(null);
  const guiState: GuiState = useMemo(
    () => ({
      selected_property: selected,
      view: showTrace ? "trace" : center === "source" ? "source" : center === "schematic" ? "schematic" : "table",
      filter,
      status_filter: statusFilter,
      layout_preset: layoutPreset,
      active_app: appsOpen ? appMode : null,
      bottom_tab: bottomTab,
      follow_agent: followAgent,
      trace: traceName
        ? {
            property: traceName,
            cursor: traceView?.cursor ?? 0,
            signals: traceView?.signals ?? [],
            why_cex_open: whyCexOpen,
            slice_only: Boolean(sliceOnlyControlled),
            annotations: traceAnnotations,
          }
        : null,
      source: srcPath ? { file: srcPath, first_line: focusLine, last_line: focusEnd ?? focusLine } : null,
      schematic: center === "schematic" || layoutPreset !== "triage" ? { property: activeSchemProp, max_depth: schematicDepth } : null,
      walkthrough: walkthrough
        ? {
            title: walkthrough.title,
            step: walkthrough.step,
            total_steps: walkthrough.steps.length,
            label: walkthrough.steps[walkthrough.step]?.label ?? "",
          }
        : null,
      focus_context: focusChips.length ? focusChips : null,
      capture: lastCapture,
    }),
    [
      selected,
      showTrace,
      center,
      filter,
      statusFilter,
      layoutPreset,
      appsOpen,
      appMode,
      bottomTab,
      followAgent,
      traceName,
      traceView,
      whyCexOpen,
      sliceOnlyControlled,
      traceAnnotations,
      srcPath,
      focusLine,
      focusEnd,
      activeSchemProp,
      schematicDepth,
      walkthrough,
      focusChips,
      lastCapture,
    ],
  );
  const guiNow = useRef(guiState);
  guiNow.current = guiState;
  useEffect(() => {
    backend.uiState(guiState).catch(() => {});
  }, [guiState]);
  // An action is answered with the window's state once the views have
  // settled on it, or with the reason it could not be done. Each one is
  // noted in Messages, so the person sees what moved their window.
  const guiDone = useCallback((id: number, what: string, error?: string) => {
    setEvents((e) => [...e, { at: new Date(), cmd: `AI window: ${what}`, ok: !error, ms: 0, summary: error ?? "" }]);
    if (error) backend.aiGuiDone(id, null, error).catch(() => {});
    else setTimeout(() => backend.aiGuiDone(id, guiNow.current).catch(() => {}), 60);
  }, []);
  // Waiting for a trace to load (open_trace / present_cex), and signals still to add.
  const guiTrace = useRef<{ id: number; path: string; what: string; capture?: boolean } | null>(null);
  const guiAdd = useRef<{ id: number; left: string[]; missing: string[]; what: string } | null>(null);
  useEffect(() => {
    const w = guiTrace.current;
    if (!w || trace?.path !== w.path || (trace.text === null && !trace.error)) return;
    guiTrace.current = null;
    if (w.capture && !trace.error) {
      setTimeout(() => {
        const svg = traceSvgCaptureRef.current?.() ?? null;
        if (svg) setLastCapture({ target: "trace", format: "svg", svg, captured_at: new Date().toISOString() });
        guiDone(w.id, w.what);
      }, 60);
      return;
    }
    guiDone(w.id, w.what, trace.error ?? undefined);
  }, [trace, guiDone, setLastCapture]);
  const guiAct = useRef<(r: GuiRequest) => void>(() => {});
  guiAct.current = ({ id, action: a }) => {
    markAgentDriven();
    if (a.action === "select_property") {
      const what = `select ${a.name}`;
      if (!allProps.some((p) => p.name === a.name)) return guiDone(id, what, `no property named ${a.name}`);
      setSelected(a.name);
      return guiDone(id, what);
    }
    if (a.action === "open_trace") {
      const what = `open the trace of ${a.name}${a.frame != null ? ` at cycle ${a.frame}` : ""}`;
      const p = allProps.find((q) => q.name === a.name);
      const path = traceOf(p ?? null);
      if (!p) return guiDone(id, what, `no property named ${a.name}`);
      if (!path) return guiDone(id, what, `${a.name} has no trace: its status is ${p.status.status}`);
      openTrace(a.name);
      if (a.frame != null) setCursorReq((r) => ({ cycle: a.frame as number, n: (r?.n ?? 0) + 1 }));
      if (trace?.path === path && trace.text !== null) return guiDone(id, what);
      guiTrace.current = { id, path, what };
      return;
    }
    if (a.action === "add_signals") {
      const what = `add ${a.signals.join(", ")} to the trace`;
      if (!traceName || !trace?.text) return guiDone(id, what, "no trace is open: open one first (gui_open_trace)");
      if (guiAdd.current) return guiDone(id, what, "signals are still being added: try again");
      const [first, ...left] = a.signals;
      guiAdd.current = { id, left, missing: [], what };
      setCenter("trace");
      setAddReq((r) => ({ path: first, n: (r?.n ?? 0) + 1 }));
      return;
    }
    if (a.action === "highlight_source") {
      const what = `show ${a.file}:${a.first_line}${a.last_line > a.first_line ? `-${a.last_line}` : ""}`;
      backend.readText(a.file).then(
        (text) => {
          setTexts((t) => ({ ...t, [a.file]: text }));
          setSrcShow({ file: a.file, first: a.first_line, last: a.last_line });
          setCenter("source");
          guiDone(id, what);
        },
        (e) => guiDone(id, what, String(e)),
      );
      return;
    }
    if (a.action === "set_view") {
      const parts: string[] = [];
      if (a.layout_preset) {
        setLayoutPreset(a.layout_preset);
        parts.push(`preset=${a.layout_preset}`);
      }
      if (a.app_tab) {
        openApps(a.app_tab as AppMode);
        parts.push(`app=${a.app_tab}`);
      }
      if (a.bottom_tab) {
        if (a.bottom_tab === "profiler") openProfiler();
        else if (a.bottom_tab === "apps") openApps();
        else setBottomTab(a.bottom_tab as BottomTab);
        parts.push(`dock=${a.bottom_tab}`);
      }
      if (a.filter != null) {
        setFilter(a.filter);
        parts.push(`filter=${a.filter}`);
      }
      if (a.status_filter !== undefined) {
        setStatusFilter((a.status_filter as StatusFilter) ?? null);
        parts.push(`status=${a.status_filter ?? "all"}`);
      }
      if (a.view) {
        if (a.view === "apps") openApps((a.app_tab as AppMode | undefined) ?? appMode);
        else if (a.view === "schematic") openSchematic();
        else setCenter(a.view as Center);
        parts.push(`view=${a.view}`);
      }
      return guiDone(id, `set view (${parts.join(", ") || "no-op"})`);
    }
    if (a.action === "toggle_why_cex") {
      const what = `${a.open ? "open" : "close"} Why CEX slice`;
      if (!traceName) return guiDone(id, what, "no trace is open: open one first (gui_open_trace)");
      if (a.slice_only != null) setSliceOnlyControlled(a.slice_only);
      if (!a.open) {
        setWhyCexOpen(false);
        return guiDone(id, what);
      }
      setWhyCexOpen(true);
      if (whyCexMap[traceName]) return guiDone(id, what);
      setWhyCexLoading(true);
      backend
        .whyCex(traceName)
        .then(
          (rep) => {
            setWhyCexMap((m) => ({ ...m, [traceName]: rep }));
            guiDone(id, what);
          },
          (e) => {
            setWhyCexOpen(false);
            guiDone(id, what, String(e));
          },
        )
        .finally(() => setWhyCexLoading(false));
      return;
    }
    if (a.action === "annotate_trace") {
      if (a.clear) {
        setTraceAnnotations([]);
        return guiDone(id, "clear trace callouts");
      }
      if (!traceName) return guiDone(id, "annotate trace", "no trace is open: open one first (gui_open_trace)");
      const list = a.annotations ?? [];
      setTraceAnnotations(list);
      return guiDone(id, `pin ${list.length} callout${list.length === 1 ? "" : "s"} on ${traceName}`);
    }
    if (a.action === "present_walkthrough") {
      if (a.clear) {
        setWalkthrough(null);
        return guiDone(id, "dismiss walkthrough");
      }
      if (a.steps && a.steps.length > 0) {
        const idx = Math.max(0, Math.min(a.steps.length - 1, Number(a.step ?? 0)));
        const title = a.title || "Root-cause walkthrough";
        setFollowAgent(true);
        setWalkthrough({ title, steps: a.steps, step: idx });
        applyWalkthroughStep(a.steps[idx]);
        return guiDone(id, `present walkthrough "${title}" (${a.steps.length} steps)`);
      }
      if (a.step != null && walkthrough) {
        const idx = Math.max(0, Math.min(walkthrough.steps.length - 1, Number(a.step)));
        setWalkthrough({ ...walkthrough, step: idx });
        if (followAgent) applyWalkthroughStep(walkthrough.steps[idx]);
        return guiDone(id, `step walkthrough to ${idx + 1}/${walkthrough.steps.length}`);
      }
      return guiDone(id, "present walkthrough", "`steps` or `step` is required");
    }
    if (a.action === "open_schematic") {
      const target = a.name ?? selected ?? activeSchemProp;
      if (!target) return guiDone(id, "open schematic", "no property available for schematic");
      const p = allProps.find((q) => q.name === target);
      if (!p) return guiDone(id, `open schematic of ${target}`, `no property named ${target}`);
      if (p.kind === "assume") return guiDone(id, `open schematic of ${target}`, "an assumption has no target cone");
      openSchematic(target, a.max_depth != null ? Number(a.max_depth) : undefined);
      return guiDone(id, `open schematic of ${target}`);
    }
    if (a.action === "present_cex") {
      const what = `present CEX walkthrough for ${a.name}`;
      const p = allProps.find((q) => q.name === a.name);
      const path = traceOf(p ?? null);
      if (!p) return guiDone(id, what, `no property named ${a.name}`);
      if (!path) return guiDone(id, what, `${a.name} has no trace: its status is ${p.status.status}`);
      setLayoutPreset(a.layout_preset ?? "cex-debug");
      openTrace(a.name);
      setSchematicTarget(a.name);
      if (a.slice_only != null) setSliceOnlyControlled(a.slice_only);
      setWhyCexOpen(true);
      if (!whyCexMap[a.name]) {
        setWhyCexLoading(true);
        backend
          .whyCex(a.name)
          .then((rep) => setWhyCexMap((m) => ({ ...m, [a.name]: rep })))
          .catch(() => {})
          .finally(() => setWhyCexLoading(false));
      }
      if (a.annotations?.length) {
        setTraceAnnotations(a.annotations);
      }
      if (a.steps && a.steps.length > 0) {
        const title = a.title || `Why ${a.name} fails`;
        setFollowAgent(true);
        setWalkthrough({ title, steps: a.steps, step: 0 });
        const st0 = a.steps[0];
        if (st0.file) {
          backend.readText(st0.file).then(
            (t) => {
              setTexts((m) => ({ ...m, [st0.file!]: t }));
              if (st0.first_line != null) {
                setSrcShow({ file: st0.file!, first: st0.first_line, last: st0.last_line ?? st0.first_line });
              }
            },
            () => {},
          );
        }
      }
      const targetFrame = a.frame ?? a.steps?.[0]?.frame ?? null;
      if (targetFrame != null) {
        setCursorReq((r) => ({ cycle: targetFrame, n: (r?.n ?? 0) + 1 }));
      }
      if (trace?.path === path && trace.text !== null) {
        if (a.capture) {
          setTimeout(() => {
            const svg = traceSvgCaptureRef.current?.() ?? null;
            if (svg) setLastCapture({ target: "trace", format: "svg", svg, captured_at: new Date().toISOString() });
            guiDone(id, what);
          }, 80);
          return;
        }
        return guiDone(id, what);
      }
      guiTrace.current = { id, path, what, capture: Boolean(a.capture) };
      return;
    }
    if (a.action === "capture_view") {
      const target = a.target ?? "trace";
      if (target === "trace") {
        const svg = traceSvgCaptureRef.current?.() ?? null;
        if (!svg) return guiDone(id, "capture trace SVG", "no trace is open to capture");
        setLastCapture({ target: "trace", format: "svg", svg, captured_at: new Date().toISOString() });
        return guiDone(id, "capture trace SVG");
      }
      if (target === "schematic") {
        const svg = schemSvgCaptureRef.current?.() ?? null;
        if (!svg) return guiDone(id, "capture schematic SVG", "no schematic is open to capture");
        setLastCapture({ target: "schematic", format: "svg", svg, captured_at: new Date().toISOString() });
        return guiDone(id, "capture schematic SVG");
      }
      const pal = resolveSvgPalette();
      const fallbackSvg =
        traceSvgCaptureRef.current?.() ??
        schemSvgCaptureRef.current?.() ??
        `<svg xmlns="http://www.w3.org/2000/svg" width="480" height="80"><rect width="480" height="80" fill="${pal.surface}"/><text x="16" y="44" fill="${pal.text}" font-family="monospace" font-size="13">InFormal window: ${allProps.length} properties (${selected ?? "none selected"})</text></svg>`;
      setLastCapture({ target: "window", format: "svg", svg: fallbackSvg, captured_at: new Date().toISOString() });
      return guiDone(id, "capture window SVG");
    }
  };
  useEffect(() => backend.onAiGui((r) => guiAct.current(r)), []);
  const onAddResult = useCallback(
    (path: string, r: "added" | "shown" | "missing") => {
      note(r !== "missing", r === "added" ? `Added ${path} to the trace` : r === "shown" ? `${path} is already in the trace` : `${path} is not in this trace`);
      // One of an agent's signals: the next one, or the answer.
      const g = guiAdd.current;
      if (!g) return;
      if (r === "missing") g.missing.push(path);
      const next = g.left.shift();
      if (next !== undefined) return setAddReq((q) => ({ path: next, n: (q?.n ?? 0) + 1 }));
      guiAdd.current = null;
      guiDone(g.id, g.what, g.missing.length ? `not in this trace: ${g.missing.join(", ")}` : undefined);
    },
    [note, guiDone],
  );

  const idle = !busy && !running;

  // ---- Design Info tab ----
  // Read when the tab is opened (it builds the current task's model, so it
  // is not redone after every command: the tab says when it is out of date).
  const [info, setInfo] = useState<{ data: DesignInfoData | null; error: string | null; of: SessionView | null }>({ data: null, error: null, of: null });
  const [infoLoading, setInfoLoading] = useState(false);
  const loadInfo = useCallback(() => {
    const of = session;
    setInfoLoading(true);
    backend.designInfo().then(
      (data) => setInfo({ data, error: null, of }),
      (e) => setInfo((i) => ({ ...i, error: String(e instanceof Error ? e.message : e), of })),
    ).finally(() => setInfoLoading(false));
  }, [session]);
  const infoOpen = bottomTab === "info";
  const wasOpen = useRef(false);
  useEffect(() => {
    if (infoOpen && !wasOpen.current && idle && session?.elaborated) loadInfo();
    wasOpen.current = infoOpen && (!!session?.elaborated || wasOpen.current);
  }, [infoOpen, idle, session?.elaborated, loadInfo]);
  const proveAll = () => provePlan.cmd && idle && prove(provePlan.cmd);
  const proveSelected = () => current && current.kind !== "assume" && idle && prove(proveOne(current.name));

  // ---- Proof Effort & Budget Controls (Dual-Persona: Novice UI + Tcl Echo) ----
  const curEffort: EffortLevel = session?.effort ?? "standard";
  const curOrch: OrchestrationMode = session?.orchestration ?? "auto";
  const curMaxBound = session?.max_trace_length ?? null;
  const formatDurShort = (d?: { secs: number; nanos: number } | null): string => {
    if (!d) return "";
    if (d.nanos !== 0) return `${Math.round(d.secs * 1000 + d.nanos / 1_000_000)}ms`;
    if (d.secs >= 3600 && d.secs % 3600 === 0) return `${d.secs / 3600}h`;
    if (d.secs >= 60 && d.secs % 60 === 0) return `${d.secs / 60}m`;
    return `${d.secs}s`;
  };
  const curPerPropText = formatDurShort(session?.time_limit) || "1h";
  const curTotalText = formatDurShort(session?.total_time_limit);
  const [budgetOpen, setBudgetOpen] = useState(false);
  const [tlDraft, setTlDraft] = useState("");
  const [totDraft, setTotDraft] = useState("");
  const [boundDraft, setBoundDraft] = useState("");
  const budgetWrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!budgetOpen) return;
    setTlDraft(curPerPropText);
    setTotDraft(curTotalText);
    setBoundDraft(curMaxBound != null ? String(curMaxBound) : "");
    const onDown = (ev: MouseEvent) => {
      if (budgetWrapRef.current && !budgetWrapRef.current.contains(ev.target as Node)) {
        setBudgetOpen(false);
      }
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        setBudgetOpen(false);
      }
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [budgetOpen, curPerPropText, curTotalText, curMaxBound]);

  const applyEffort = useCallback(
    (eff: EffortLevel) => {
      if (!idle) return;
      const cmd = flavor === "vcf" ? `set_fml_var fml_effort ${eff}` : `set_prove_effort ${eff}`;
      void run(() => backend.evalTcl(cmd), cmd);
    },
    [idle, flavor, run],
  );
  const applyPerPropLimit = useCallback(
    (val: string) => {
      const v = val.trim();
      if (!idle || !v) return;
      const cmd = flavor === "vcf" ? `set_fml_var fml_max_time ${v}` : `set_prove_time_limit ${v}`;
      setTlDraft(v);
      void run(() => backend.evalTcl(cmd), cmd);
    },
    [idle, flavor, run],
  );
  const applyTotalLimit = useCallback(
    (val: string) => {
      const v = val.trim() || "off";
      if (!idle) return;
      const cmd = `set_prove_total_time_limit ${v}`;
      setTotDraft(v === "off" || v === "0" ? "" : v);
      void run(() => backend.evalTcl(cmd), cmd);
    },
    [idle, run],
  );
  const applyMaxBound = useCallback(
    (val: string) => {
      const v = val.trim() || "off";
      if (!idle) return;
      const cmd = flavor === "vcf" ? `set_fml_var fml_max_depth ${v}` : `set_max_trace_length ${v}`;
      setBoundDraft(v === "off" || v === "0" ? "" : v);
      void run(() => backend.evalTcl(cmd), cmd);
    },
    [idle, flavor, run],
  );
  const applyOrchestration = useCallback(
    (mode: OrchestrationMode) => {
      if (!idle) return;
      const cmd = `set_prove_orchestration ${mode}`;
      void run(() => backend.evalTcl(cmd), cmd);
    },
    [idle, run],
  );
  const openScriptPath = useCallback(
    (p: string) => {
      setScriptPath(p);
      recordRecentSession(p);
      if (idle) void run(() => backend.source(p), `source ${p}`);
    },
    [idle, recordRecentSession, run],
  );

  const runScript = () => {
    if (!scriptPath) {
      pathRef.current?.focus();
      return;
    }
    recordRecentSession(scriptPath);
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
    else if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "k") {
      setCommandPaletteOpen(!commandPaletteOpen);
    }
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
            title: cmd ? undefined : "The JasperGold flavor of the informal shell has no disable command yet; switch to VC Formal (fvdisable)",
            onClick: () => cmd && run(() => backend.evalTcl(cmd), cmd),
          };
        })(),
      ]
    : [];

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
    const h0 = activeBottomH;
    drag(e, (ev) => setActiveBottomH(clampBottom(h0 + y0 - ev.clientY, window.innerHeight)));
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
    setActiveBottomH(clampBottom(activeBottomH + d, window.innerHeight));
  };

  const menus: MenuDef[] = [
    {
      label: "File",
      items: [
        { label: "Source script…", onClick: runScript },
        { label: "Edit script path", onClick: () => pathRef.current?.focus() },
        { label: "Command palette…", keys: "Ctrl+K", onClick: () => setCommandPaletteOpen(true) },
        { sep: true },
        { label: "Close design", disabled: true },
      ],
    },
    { label: "Edit", items: [{ label: "Find property", keys: "Ctrl+F", onClick: focusFilter }, { label: "Clear filters", onClick: () => { setFilter(""); setStatusFilter(null); setTaskFilter(null); } }, { sep: true }, { label: "Console prompt", keys: "Ctrl+L", onClick: focusConsole }] },
    {
      label: "View",
      items: [
        {
          label: "Task / Design dock",
          checked: showLeft,
          onClick: () => (narrow ? setDrawerOpen((o) => !o) : setLeftOpen((o) => !o)),
        },
        { label: "Property table", checked: center === "table" && layoutPreset === "triage", onClick: () => { setLayoutPreset("triage"); setCenter("table"); } },
        { label: `Source (${fileName ?? "none"})`, checked: center === "source" && layoutPreset === "triage", keys: "F3", onClick: () => { setLayoutPreset("triage"); setCenter("source"); }, disabled: !topFile },
        { label: "COI & causal schematic", checked: center === "schematic" && layoutPreset === "triage", onClick: () => { setLayoutPreset("triage"); openSchematic(); }, disabled: !allProps.length },
        { sep: true },
        { label: "Layout: Triage (Default)", checked: layoutPreset === "triage", onClick: () => setLayoutPreset("triage") },
        { label: "Layout: CEX Debug (Source + Trace + Schematic)", checked: layoutPreset === "cex-debug", onClick: () => setLayoutPreset("cex-debug") },
        { label: "Layout: Split Workspace", checked: layoutPreset === "split", onClick: () => setLayoutPreset("split") },
        { label: "Layout: Signoff & Apps", checked: layoutPreset === "signoff", onClick: () => setLayoutPreset("signoff") },
        { sep: true },
        { label: "Console", keys: "Ctrl+1", checked: bottomTab === "console", onClick: () => setBottomTab("console") },
        { label: "Details", keys: "Ctrl+2", checked: bottomTab === "details", onClick: () => setBottomTab("details") },
        { label: "Messages", keys: "Ctrl+3", checked: bottomTab === "messages", onClick: () => setBottomTab("messages") },
        { label: "Log", keys: "Ctrl+4", checked: bottomTab === "log", onClick: () => setBottomTab("log") },
        { label: "Profiler", keys: "Ctrl+5", checked: profiler && bottomTab === "profiler", onClick: openProfiler },
        { label: "Design Info", checked: bottomTab === "info", onClick: () => setBottomTab("info") },
        { label: "Table columns…", onClick: () => setColMenu({ x: Math.max(8, window.innerWidth - 220), y: 80 }) },
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
    {
      label: "Design",
      items: [
        {
          label: "Show hierarchy",
          onClick: () => {
            if (narrow) setDrawerOpen(true);
            else setLeftOpen(true);
            setLeft("design");
          },
        },
        { label: "Design info", onClick: () => setBottomTab("info") },
        { label: "Elaborate", disabled: true },
      ],
    },
    {
      label: "Prove",
      items: [
        { label: "Prove all", hint: proveCmd, keys: "F5", onClick: proveAll, disabled: !idle || !allProps.length },
        { label: "Prove selected", keys: "F6", onClick: proveSelected, disabled: !idle || !current || current.kind === "assume" },
        { label: stopping ? "Stopping…" : "Stop", keys: "Esc", onClick: stop, disabled: !running || stopping },
        { sep: true },
        { label: "Effort: Quick (low)", checked: curEffort === "low", disabled: !idle, onClick: () => applyEffort("low") },
        { label: "Effort: Standard", checked: curEffort === "standard", disabled: !idle, onClick: () => applyEffort("standard") },
        { label: "Effort: Deep (high)", checked: curEffort === "high", disabled: !idle, onClick: () => applyEffort("high") },
        { label: "Effort: Signoff (max)", checked: curEffort === "max", disabled: !idle, onClick: () => applyEffort("max") },
        { sep: true },
        { label: "Proof budget & limits…", disabled: !idle, onClick: () => setBudgetOpen(true) },
      ],
    },
    {
      label: "Debug",
      items: [
        { label: "Visualize trace", disabled: !hasTrace, onClick: () => current && hasTrace && openTrace(current.name) },
        { label: "Show source of selected", keys: "F3", onClick: () => current && setCenter("source"), disabled: !current || !topFile },
        { label: "Open COI & causal schematic", onClick: () => openSchematic(current?.name), disabled: !allProps.length },
        { label: "✦ Explain & walk through CEX", onClick: () => current && hasTrace && void launchCexWalkthrough(current.name), disabled: !hasTrace },
      ],
    },
    {
      label: "Tools",
      items: [
        { label: "Command palette…", keys: "Ctrl+K", onClick: () => setCommandPaletteOpen(true) },
        { label: "Proof profiler", keys: "Ctrl+5", checked: profiler, onClick: () => (profiler ? closeProfiler() : openProfiler()) },
        { label: "Formal apps", checked: appsOpen, onClick: () => (appsOpen ? closeApps() : openApps()) },
      ],
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
        { label: "InFormal v0.2", disabled: true },
      ],
    },
  ];

  const paletteCommands: PaletteCommand[] = useMemo(
    () => [
      { id: "prove-all", group: "Prove", label: "Prove all properties", shortcut: "F5", detail: proveCmd, disabled: !idle || !allProps.length, run: proveAll },
      ...(current && current.kind !== "assume"
        ? [{ id: "prove-sel", group: "Prove", label: `Prove selected: ${current.name}`, shortcut: "F6", disabled: !idle, run: proveSelected }]
        : []),
      ...(current && hasTrace
        ? [
            { id: "walk-cex", group: "AI Co-Pilot", label: `✦ Explain & walk through CEX: ${current.name}`, detail: "why_cex + 3-way tour", run: () => void launchCexWalkthrough(current.name) },
            { id: "vis-trace", group: "Debug", label: `Open waveform trace: ${current.name}`, run: () => openTrace(current.name) },
          ]
        : []),
      { id: "view-table", group: "View", label: "Show Property Table", run: () => { setLayoutPreset("triage"); setCenter("table"); } },
      { id: "view-source", group: "View", label: "Show RTL Source (CodeMirror 6)", shortcut: "F3", disabled: !topFile, run: () => { setLayoutPreset("triage"); setCenter("source"); } },
      { id: "view-schem", group: "View", label: "Show COI & Causal Schematic", disabled: !allProps.length, run: () => { setLayoutPreset("triage"); openSchematic(); } },
      { id: "preset-triage", group: "Layout", label: "Workspace Preset: Triage (Single-pane)", run: () => setLayoutPreset("triage") },
      { id: "preset-cex", group: "Layout", label: "Workspace Preset: CEX Debug (Source + Trace + Schematic)", run: () => setLayoutPreset("cex-debug") },
      { id: "preset-split", group: "Layout", label: "Workspace Preset: Side-by-Side Split", run: () => setLayoutPreset("split") },
      { id: "preset-signoff", group: "Layout", label: "Workspace Preset: Signoff & Formal Apps", run: () => setLayoutPreset("signoff") },
      { id: "app-cov", group: "Formal Apps", label: "Open COV / UNR Coverage", run: () => openApps("cov") },
      { id: "app-xprop", group: "Formal Apps", label: "Open X-Prop Verification", run: () => openApps("xprop") },
      { id: "app-spv", group: "Formal Apps", label: "Open SPV Security Paths", run: () => openApps("spv") },
      { id: "app-sec", group: "Formal Apps", label: "Open SEC / LEC Equivalence", run: () => openApps("sec") },
      { id: "app-csr", group: "Formal Apps", label: "Open CSR / RAL Verification", run: () => openApps("csr") },
      { id: "app-proto", group: "Formal Apps", label: "Open Bus Protocol VIP", run: () => openApps("protocol") },
      { id: "theme-light", group: "Theme", label: "Switch Theme: Light", run: () => setTheme("light") },
      { id: "theme-dark", group: "Theme", label: "Switch Theme: Dark", run: () => setTheme("dark") },
      { id: "flavor-jg", group: "Flavor", label: "Tcl Flavor: JasperGold", disabled: !idle, run: () => changeFlavor("jg") },
      { id: "flavor-vcf", group: "Flavor", label: "Tcl Flavor: VC Formal", disabled: !idle, run: () => changeFlavor("vcf") },
      { id: "effort-low", group: "Prove", label: "Proof Effort: Quick (low)", disabled: !idle, run: () => applyEffort("low") },
      { id: "effort-standard", group: "Prove", label: "Proof Effort: Standard", disabled: !idle, run: () => applyEffort("standard") },
      { id: "effort-high", group: "Prove", label: "Proof Effort: Deep (high)", disabled: !idle, run: () => applyEffort("high") },
      { id: "effort-max", group: "Prove", label: "Proof Effort: Signoff (max)", disabled: !idle, run: () => applyEffort("max") },
      ...allProps.slice(0, 80).map((p) => ({
        id: `prop-${p.name}`,
        group: "Property",
        label: p.name,
        detail: `${STATUS_LABEL[p.status.status]} · ${p.expr}`,
        run: () => (traceOf(p) ? openTrace(p.name) : select(p.name)),
      })),
    ],
    [proveCmd, idle, allProps, current, hasTrace, topFile, launchCexWalkthrough, openSchematic, openApps, setLayoutPreset, setCenter, setTheme, applyEffort],
  );

  const centerTabs = (
    <Tabs
      tabs={[
        ["table", "Property Table"],
        ...(topFile ? ([["source", fileName ?? "Source"]] as [string, string][]) : []),
        ...(allProps.length ? ([["schematic", "Schematic"]] as [string, string][]) : []),
        ...(traceName ? ([["trace", `Trace: ${traceName}`, closeTrace]] as [string, string, () => void][]) : []),
      ]}
      value={center === "trace" && !traceName ? "table" : center}
      onChange={(t) => setCenter(t as Center)}
    />
  );

  const isDark =
    theme === "dark" ||
    (theme === "system" &&
      typeof document !== "undefined" &&
      (document.documentElement.dataset.theme === "dark" ||
        (document.documentElement.dataset.theme !== "light" &&
          typeof window !== "undefined" &&
          Boolean(window.matchMedia?.("(prefers-color-scheme: dark)").matches))));

  const traceElement = traceProp ? (
    <TraceView
      tabs={layoutPreset === "triage" && !POPOUT_KIND ? centerTabs : undefined}
      prop={traceProp}
      path={trace?.path ?? tracePath ?? ""}
      text={trace?.text ?? null}
      binary={trace?.binary ?? null}
      error={trace?.error}
      dark={isDark}
      addRequest={addReq}
      onAddResult={onAddResult}
      cursorRequest={cursorReq}
      onView={setTraceView}
      clock={session?.clocks[0] ?? null}
      whyCex={traceName ? (whyCexMap[traceName] ?? null) : null}
      whyCexOpen={whyCexOpen}
      whyCexLoading={whyCexLoading}
      onToggleWhyCex={toggleWhyCex}
      annotations={traceAnnotations}
      controlledSliceOnly={sliceOnlyControlled}
      onSliceOnlyChange={setSliceOnlyControlled}
      onPinContext={pinContext}
      onPopOut={!POPOUT_KIND ? () => void backend.popOutWindow("trace", `InFormal Trace — ${traceProp.name}`) : undefined}
      onCaptureSvgRef={traceSvgCaptureRef}
      actions={
        <span className="surfer-wrap" title={surferTip(tracePath)}>
          <button className="btn" disabled={!surfer || !tracePath} onClick={() => tracePath && openInSurfer(tracePath)} title={surferTip(tracePath)}>
            Open in Surfer
          </button>
        </span>
      }
    />
  ) : (
    <p className="empty">Double-click a CEX or covered property to visualize its trace.</p>
  );

  const sourceElement = (
    <SourceView
      text={source}
      file={srcPath}
      switchFiles={sourceSwitchFiles}
      onSelectFile={(f) => {
        setSrcShow(null);
        setSrcFileOverride(f);
        loadText(f);
      }}
      focus={focusLine}
      focusEnd={focusEnd}
      causalSignals={whyCexOpen && traceName ? whyCexMap[traceName]?.active_signals : undefined}
      covItems={session?.cov?.items?.filter((it) => !it.file || it.file === srcPath || Boolean(srcPath?.endsWith(`/${it.file}`)))}
      lintFindings={lintFindings.filter((f) => !srcPath || f.file === srcPath || Boolean(srcPath.endsWith(`/${f.file}`)))}
      onApplyFix={applyQuickFix}
      onJumpLocation={(f, line) => {
        backend.readText(f).then(
          (t) => {
            setTexts((m) => ({ ...m, [f]: t }));
            setSrcShow({ file: f, first: line, last: line });
            if (layoutPreset === "triage") setCenter("source");
          },
          () => {},
        );
      }}
      onPinContext={pinContext}
    />
  );

  const schematicElement = (
    <SchematicView
      tabs={layoutPreset === "triage" && !POPOUT_KIND ? centerTabs : undefined}
      graph={schematicGraph}
      loading={schematicLoading}
      error={schematicError}
      maxDepth={schematicDepth}
      dark={isDark}
      onChangeDepth={(d) => setSchematicTarget(activeSchemProp, d)}
      onSelectSignalSource={(sig) => {
        if (!topFile) return;
        const leaf = sig.split(".").pop() ?? sig;
        const idx = topText.split("\n").findIndex((ln) => new RegExp(`\\b${leaf}\\b`).test(ln));
        setSrcShow({ file: topFile, first: idx >= 0 ? idx + 1 : 1, last: idx >= 0 ? idx + 1 : 1 });
        if (layoutPreset === "triage") setCenter("source");
      }}
      onAddSignalToTrace={(sig) => addSignal(sig)}
      onPinContext={pinContext}
      onPopOut={!POPOUT_KIND ? () => void backend.popOutWindow("schematic", `InFormal Schematic — ${activeSchemProp ?? ""}`) : undefined}
      onCaptureSvgRef={schemSvgCaptureRef}
    />
  );

  // Popped-out secondary monitor window mode (`?popout=trace` or `?popout=schematic`)
  if (POPOUT_KIND) {
    return (
      <div className="app popout-window">
        <main className="center" style={{ height: "100%" }}>
          {POPOUT_KIND === "trace" ? traceElement : schematicElement}
        </main>
      </div>
    );
  }

  const showWelcome = !session?.elaborated && allProps.length === 0 && !busy && !running;

  return (
    <div
      className={`app ${backend.inMcpApp ? "mcp-app-mode" : ""} ${IS_MAC_OVERLAY ? "mac-overlay-titlebar" : ""}`}
      style={{ "--bottom-h": `${activeBottomH}px`, ...(leftW ? { "--left-w": `${leftW}px` } : {}) } as React.CSSProperties}
    >
      {/* ---- menu bar ---- */}
      {!backend.inMcpApp && (
        <MenuBar menus={menus} macOverlay={IS_MAC_OVERLAY}>
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
          <button
            className="btn small palette-trigger"
            onClick={() => setCommandPaletteOpen(true)}
            title="Open Command Palette (Ctrl+K / Cmd+K)"
            aria-label="Command palette"
          >
            ⌘K
          </button>
        </MenuBar>
      )}

      {/* ---- tool row ---- */}
      <div className={`toolrow ${running ? "is-proving" : ""}`}>
        {narrow && (
          <button
            className={`tool drawer-toggle ${drawerOpen ? "on" : ""}`}
            onClick={() => setDrawerOpen((o) => !o)}
            aria-expanded={drawerOpen}
            aria-label="Toggle tasks sidebar"
            title="Toggle Tasks / Design sidebar"
          >
            ☰
          </button>
        )}
        <label className="field">
          <span>App</span>
          <select
            aria-label="App"
            defaultValue="fpv"
            onChange={(e) => {
              const v = e.target.value;
              if (v !== "fpv") openApps(v as AppMode);
            }}
          >
            <option value="fpv">FPV</option>
            <option value="sec">SEC / LEC</option>
            <option value="cov">COV / UNR</option>
            <option value="xprop">X-Prop</option>
            <option value="spv">SPV</option>
            <option value="csr">CSR</option>
            <option value="protocol">Protocol VIP</option>
          </select>
        </label>
        <label className="field">
          <span>Flavor</span>
          <select value={flavor} disabled={!idle} onChange={(e) => changeFlavor(e.target.value as Flavor)} aria-label="Flavor">
            <option value="jg">JasperGold</option>
            <option value="vcf">VC Formal</option>
          </select>
        </label>
        <label className="field layout-field">
          <span>Layout</span>
          <select
            aria-label="Layout"
            value={layoutPreset}
            onChange={(e) => setLayoutPreset(e.target.value as LayoutPreset)}
          >
            <option value="triage">Triage</option>
            <option value="cex-debug">CEX Debug</option>
            <option value="split">Split</option>
            <option value="signoff">Signoff</option>
          </select>
        </label>
        <span className="vsep" />
        <ToolBtn title={scriptPath ? `source ${scriptPath}` : "Run script (set a script path first)"} onClick={runScript} disabled={!idle}>
          <ScriptIcon />
        </ToolBtn>
        <button
          className="tool tool-text"
          onClick={proveAll}
          disabled={!idle || !provePlan.cmd}
          title={provePlan.title}
          aria-label="Prove all"
        >
          <PlayIcon /> Prove all{provePlan.badge ? <span className="prove-target-badge">{provePlan.badge}</span> : null}
        </button>
        <button className="tool tool-text" onClick={stop} disabled={!running || stopping} title="Stop the running prove (Esc)">
          <StopIcon /> {stopping ? "Stopping…" : "Stop"}
        </button>
        <div className="prove-config-group" ref={budgetWrapRef}>
          <label
            className="field effort-field"
            title="Engine proof effort: Quick (low), Standard, Deep (high: invariant mining + deep CTG), Signoff (max: 6-worker IC3 + witness & proof core)"
          >
            <span className="effort-label">Effort</span>
            <select
              aria-label="Proof effort"
              value={curEffort}
              disabled={!idle}
              onChange={(e) => applyEffort(e.target.value as EffortLevel)}
            >
              <option value="low">Quick</option>
              <option value="standard">Standard</option>
              <option value="high">Deep</option>
              <option value="max">Signoff</option>
            </select>
          </label>
          <button
            type="button"
            className={`tool tool-text budget-btn ${budgetOpen ? "on" : ""}`}
            disabled={!idle}
            aria-label="Proof budget"
            aria-expanded={budgetOpen}
            title="Configure per-property timeout, batch time limit, max trace bound, and 2-pass orchestration"
            onClick={() => setBudgetOpen((o) => !o)}
          >
            <span className="mono budget-summary">
              {curPerPropText}
              {curMaxBound != null ? ` · k≤${curMaxBound}` : ""}
            </span>
          </button>
          {budgetOpen && (
            <div className="budget-popover" role="dialog" aria-label="Proof budget settings">
              <div className="budget-row">
                <div className="budget-head">
                  <span className="strong">Per-prop limit</span>
                  <code className="mono dim small">{flavor === "vcf" ? "fml_max_time" : "set_prove_time_limit"}</code>
                </div>
                <div className="budget-controls">
                  {(["10s", "60s", "5m", "1h"] as const).map((p) => (
                    <button
                      key={p}
                      type="button"
                      className={`btn small ${curPerPropText === p ? "active" : ""}`}
                      disabled={!idle}
                      onClick={() => applyPerPropLimit(p)}
                    >
                      {p}
                    </button>
                  ))}
                  <input
                    className="budget-input mono"
                    aria-label="Per-property limit"
                    disabled={!idle}
                    value={tlDraft}
                    placeholder="e.g. 30s"
                    onChange={(e) => setTlDraft(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && applyPerPropLimit(tlDraft)}
                  />
                </div>
              </div>
              {flavor === "jg" && (
                <div className="budget-row">
                  <div className="budget-head">
                    <span className="strong">Total limit</span>
                    <code className="mono dim small">set_prove_total_time_limit</code>
                  </div>
                  <div className="budget-controls">
                    {(["off", "2m", "10m", "30m"] as const).map((p) => {
                      const active = p === "off" ? !curTotalText : curTotalText === p;
                      return (
                        <button
                          key={p}
                          type="button"
                          className={`btn small ${active ? "active" : ""}`}
                          disabled={!idle}
                          onClick={() => applyTotalLimit(p)}
                        >
                          {p === "off" ? "Off" : p}
                        </button>
                      );
                    })}
                    <input
                      className="budget-input mono"
                      aria-label="Total batch limit"
                      disabled={!idle}
                      value={totDraft}
                      placeholder="off"
                      onChange={(e) => setTotDraft(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && applyTotalLimit(totDraft)}
                    />
                  </div>
                </div>
              )}
              <div className="budget-row">
                <div className="budget-head">
                  <span className="strong">Max bound</span>
                  <code className="mono dim small">{flavor === "vcf" ? "fml_max_depth" : "set_max_trace_length"}</code>
                </div>
                <div className="budget-controls">
                  {(["off", "20", "50", "200"] as const).map((b) => {
                    const active = b === "off" ? curMaxBound == null : String(curMaxBound) === b;
                    return (
                      <button
                        key={b}
                        type="button"
                        className={`btn small ${active ? "active" : ""}`}
                        disabled={!idle}
                        onClick={() => applyMaxBound(b)}
                      >
                        {b === "off" ? "∞" : b}
                      </button>
                    );
                  })}
                  <input
                    className="budget-input mono"
                    aria-label="Max trace bound"
                    disabled={!idle}
                    value={boundDraft}
                    placeholder="∞"
                    onChange={(e) => setBoundDraft(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && applyMaxBound(boundDraft)}
                  />
                </div>
              </div>
              {flavor === "jg" && (
                <div className="budget-row">
                  <div className="budget-head">
                    <span className="strong">2-pass orchestration</span>
                    <code className="mono dim small">set_prove_orchestration</code>
                  </div>
                  <div className="budget-controls">
                    {(
                      [
                        ["auto", "Auto"],
                        ["on", "On"],
                        ["off", "Off"],
                      ] as const
                    ).map(([val, label]) => (
                      <button
                        key={val}
                        type="button"
                        className={`btn small ${curOrch === val ? "active" : ""}`}
                        disabled={!idle}
                        onClick={() => applyOrchestration(val)}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
        <span className="vsep" />
        <ToolBtn
          title={hasTrace ? `Visualize trace (${currentTrace})` : "Visualize trace: select a CEX or cover"}
          disabled={!hasTrace}
          on={showTrace && traceName === current?.name}
          onClick={() => current && openTrace(current.name)}
        >
          <WaveIcon />
        </ToolBtn>
        <ToolBtn title="Source view (F3)" onClick={toggleSource} disabled={!topFile} on={center === "source" && layoutPreset === "triage"}>
          <CodeIcon />
        </ToolBtn>
        <ToolBtn title="COI & causal schematic" onClick={() => openSchematic()} disabled={!allProps.length} on={center === "schematic" && layoutPreset === "triage"}>
          <SchematicIcon />
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
              {(() => {
                const r = progress.props.find((q) => q.status === "running");
                return r ? ` · ${r.name}${r.engine ? ` ${r.engine}` : ""}${r.depth != null ? ` k=${r.depth}` : ""} ${clock(r.ms ?? 0)}` : "";
              })()}
            </span>
          </span>
        )}
        {!backend.inTauri && <span className="muted small">{backend.inMcpApp ? "Antigravity MCP App" : "Browser preview"}</span>}
      </div>

      {walkthrough && (
        <WalkthroughBar
          walkthrough={walkthrough}
          followAgent={followAgent}
          onStep={(nextIdx, st) => {
            setWalkthrough({ ...walkthrough, step: nextIdx });
            if (followAgent) applyWalkthroughStep(st);
          }}
          onToggleFollow={() => setFollowAgent(!followAgent)}
          onClose={() => setWalkthrough(null)}
        />
      )}

      <div className={`workbench ${narrow ? "narrow" : leftOpen ? "" : "left-closed"}`}>
        {narrow && drawerOpen && (
          <div className="drawer-backdrop" onClick={() => setDrawerOpen(false)} aria-hidden="true" />
        )}
        {/* ---- left dock ---- */}
        {showLeft && (
          <aside className={`left ${narrow ? "drawer" : ""}`} ref={leftRef}>
            {!narrow && (
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
            )}
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
                  onFilter={(f) => {
                    setStatusFilter(f);
                    setHiddenNotice(null);
                  }}
                  props={allProps}
                  selected={selected}
                  onSelect={selectAndRevealProp}
                  onOpen={(n) => {
                    selectAndRevealProp(n);
                    if (traceOf(allProps.find((p) => p.name === n) ?? null)) openTrace(n);
                    else select(n);
                  }}
                  tasks={tasks}
                  current={session?.current_task}
                  task={shownTask}
                  onTask={(t) => {
                    setTaskFilter(t);
                    setHiddenNotice(null);
                  }}
                />
              ) : (
                <DesignTree scopes={scopes} files={session?.design.files ?? []} fallback={!hierarchy?.length} onAddSignal={addSignal} />
              )}
            </div>
          </aside>
        )}

        {/* ---- centre ---- */}
        <main className="center">
          {showWelcome ? (
            <WelcomePane
              ai={ai}
              recent={recentSessions}
              onOpenScript={openScriptPath}
              onOpenPalette={() => setCommandPaletteOpen(true)}
            />
          ) : layoutPreset !== "triage" ? (
            <WorkbenchDock
              preset={layoutPreset}
              dark={isDark}
              slots={{
                table: (
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
                    hidden={hidden}
                    onHeaderContext={(x, y) => setColMenu({ x, y })}
                  />
                ),
                source: sourceElement,
                trace: traceElement,
                schematic: schematicElement,
                apps: (
                  <FormalApps
                    session={session}
                    mode={appMode}
                    onMode={setAppMode}
                    idle={idle}
                    onRunTcl={(cmd) => (isProveCmd(cmd) ? prove(cmd) : run(() => backend.evalTcl(cmd), cmd))}
                    onSelectTask={(t) => {
                      setTaskFilter(t);
                      setLayoutPreset("triage");
                      setCenter("table");
                    }}
                  />
                ),
              }}
            />
          ) : showTrace ? (
            traceElement
          ) : center === "schematic" ? (
            schematicElement
          ) : (
            <>
              <div className="strip">
                {centerTabs}
                {center !== "source" ? (
                  <>
                    <span className="strip-summary" title="Click a count to filter the property table">
                      <button
                        type="button"
                        className={`summary-pill ${!statusFilter ? "on" : ""}`}
                        onClick={() => {
                          setStatusFilter(null);
                          setHiddenNotice(null);
                        }}
                      >
                        {allProps.length} properties
                      </button>
                      {summaryPills.map((seg) => (
                        <span key={seg.text} style={{ display: "inline-flex", alignItems: "center" }}>
                          <span className="summary-sep">{" · "}</span>
                          <button
                            type="button"
                            className={`summary-pill tone-${seg.tone} ${statusFilter === seg.filter ? "on" : ""}`}
                            onClick={() => {
                              setStatusFilter((cur) => (cur === seg.filter ? null : seg.filter));
                              setHiddenNotice(null);
                            }}
                            title={`Filter by ${seg.text}`}
                          >
                            {seg.text}
                          </button>
                        </span>
                      ))}
                    </span>
                    <span className="spacer" />
                    {(envSummary.assumeCount > 0 || envSummary.state === "conflict") && (
                      <button
                        type="button"
                        className={`env-pill env-${envSummary.state}`}
                        title={envSummary.detail}
                        onClick={() => {
                          setStatusFilter((cur) => (cur === "assume" ? null : "assume"));
                          setHiddenNotice(null);
                        }}
                      >
                        {envSummary.state === "conflict"
                          ? "Env: conflict!"
                          : envSummary.state === "alive"
                            ? `Env: alive · ${envSummary.assumeCount} assumes`
                            : `${envSummary.assumeCount} assumes`}
                      </button>
                    )}
                    {filterChips.map((chip) => (
                      <button
                        key={chip.kind}
                        type="button"
                        className="filter-chip"
                        aria-label={chip.ariaLabel}
                        title={chip.title}
                        onClick={() => {
                          if (chip.kind === "status") setStatusFilter(null);
                          else if (chip.kind === "task") setTaskFilter(null);
                          else setFilter("");
                          setHiddenNotice(null);
                        }}
                      >
                        {chip.label} <span aria-hidden="true">×</span>
                      </button>
                    ))}
                    {filterChips.length > 0 && (
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
                    <button
                      className="btn small"
                      aria-haspopup="menu"
                      title="Show or hide table columns (also: right-click the header)"
                      onClick={(e) => {
                        const r = e.currentTarget.getBoundingClientRect();
                        setColMenu({ x: r.left, y: r.bottom + 2 });
                      }}
                    >
                      Columns
                    </button>
                  </>
                ) : (
                  <span className="strip-summary mono">{srcPath}{focusLine ? `:${focusLine}` : ""}</span>
                )}
              </div>
              {center !== "source" && hiddenNotice && (
                <div className="ux-banner tone-verdict" role="status" aria-live="polite">
                  <span className="ux-banner-text">{hiddenNotice.text}</span>
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => selectAndRevealProp(hiddenNotice.props[0].name)}
                  >
                    Show in table
                  </button>
                  <button
                    type="button"
                    className="ux-banner-close"
                    aria-label="Dismiss verdict notice"
                    onClick={() => setHiddenNotice(null)}
                  >
                    ×
                  </button>
                </div>
              )}
              {center !== "source" && !hiddenNotice && !hintDismissed && !backend.inMcpApp && guidedHint && (
                <div className={`ux-banner tone-${guidedHint.tone}`} role="status">
                  <span className="ux-banner-text">{guidedHint.text}</span>
                  {guidedHint.actionKind === "prove_all" && (
                    <button
                      type="button"
                      className="btn small"
                      disabled={!idle}
                      onClick={proveAll}
                    >
                      Run proof ({provePlan.count})
                    </button>
                  )}
                  {guidedHint.actionKind === "open_cex" && guidedHint.actionProp && (
                    <button
                      type="button"
                      className="btn small"
                      onClick={() => {
                        selectAndRevealProp(guidedHint.actionProp!);
                        if (traceOf(allProps.find((p) => p.name === guidedHint.actionProp) ?? null)) {
                          openTrace(guidedHint.actionProp!);
                        }
                      }}
                    >
                      {guidedHint.actionLabel}
                    </button>
                  )}
                  <button
                    type="button"
                    className="ux-banner-close"
                    aria-label="Dismiss guidance"
                    onClick={() => setHintDismissed(true)}
                  >
                    ×
                  </button>
                </div>
              )}
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
                  hidden={hidden}
                  onHeaderContext={(x, y) => setColMenu({ x, y })}
                />
              ) : (
                sourceElement
              )}
            </>
          )}
        </main>

        {/* ---- bottom dock ---- */}
        <section className="bottom">
          <div
            className="splitter"
            onPointerDown={startResize}
            onDoubleClick={() => setActiveBottomH(layoutPreset === "cex-debug" ? CEX_BOTTOM_DEFAULT : BOTTOM_DEFAULT)}
            onKeyDown={bottomKeys}
            tabIndex={0}
            role="separator"
            aria-orientation="horizontal"
            aria-label="Resize bottom dock"
            aria-valuenow={activeBottomH}
            title="Drag to resize; double-click to reset"
          />
          <Tabs
            tabs={[
              ["console", "Console"],
              ["details", current ? `Details: ${current.name}` : "Details"],
              ["messages", `Messages${events.some((e) => !e.ok) ? ` (${events.filter((e) => !e.ok).length})` : ""}`],
              ["log", "Log"],
              ["info", "Design Info"],
              ...(profiler ? ([["profiler", "Profiler", closeProfiler]] as [string, string, () => void][]) : []),
              ...(appsOpen ? ([["apps", "Formal Apps", closeApps]] as [string, string, () => void][]) : []),
              ...(ai?.proposals.length ? ([["proposals", `Proposals${waiting ? ` (${waiting})` : ""}`]] as [string, string][]) : []),
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
          {bottomTab === "console" && focusChips.length > 0 && (
            <div className="focus-chip-bar" role="region" aria-label="Pinned AI context">
              <span className="small muted">AI Context:</span>
              {focusChips.map((chip) => (
                <span key={chip.id} className="focus-chip mono small" title={chip.detail}>
                  {chip.label}
                  <button type="button" onClick={() => removeFocusChip(chip.id)} aria-label={`Remove ${chip.label}`}>
                    ×
                  </button>
                </span>
              ))}
              <button type="button" className="link small" onClick={clearFocusChips}>
                clear
              </button>
            </div>
          )}
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
                    <dd className="mono">{taskOf(current)}</dd>
                    {current.stats && current.status.status !== "running" && (
                      <>
                        <dt title="Process CPU time while the property ran, engine processes included; can exceed the wall time">CPU</dt>
                        <dd className="mono">{current.stats.cpu_ms != null ? `${(current.stats.cpu_ms / 1000).toFixed(2)}s` : "not measured"}</dd>
                        <dt title="trace: cycles to the failure or cover; k: k-induction; frames: IC3 frames; bound: BMC bound reached">Depth</dt>
                        <dd className="mono">{current.stats.depth != null ? `${current.stats.depth} (${current.stats.depth_kind})` : "not measured"}{current.stats.max_bound != null ? ` · deepest bound ${current.stats.max_bound}` : ""}</dd>
                        <dt title="What the winning engine gave">Evidence</dt>
                        <dd className="mono">
                          {current.stats.evidence ?? "none given"}
                          {current.stats.invariant_clauses != null ? ` · ${current.stats.invariant_clauses} clause${current.stats.invariant_clauses === 1 ? "" : "s"}` : ""}
                        </dd>
                        <dt title="Only when the high-water mark rose during this property's run">Peak mem</dt>
                        <dd className="mono">{current.stats.peak_rss_kb != null ? memText(current.stats.peak_rss_kb) : "not measured"}</dd>
                        {current.stats.certificate && (
                          <>
                            <dt>Certificate</dt>
                            <dd className="mono" title={current.stats.certificate}>{current.stats.certificate}</dd>
                          </>
                        )}
                      </>
                    )}
                    {current.kind === "assert" && current.status.status === "proven" && (
                      <>
                        <dt>Vacuity</dt>
                        <dd>{(current.vacuity ?? "not checked").replace(/-/g, " ")}</dd>
                      </>
                    )}
                    {current.ai && (
                      <>
                        <dt>Origin</dt>
                        <dd>Added through the AI interface</dd>
                      </>
                    )}
                    {!!current.under_ai?.length && (
                      <>
                        <dt title="AI-added assumptions and cut points that were in the model when this result was found">Under</dt>
                        <dd className="mono" title={current.under_ai.join(", ")}>{current.under_ai.join(", ")} (AI-added)</dd>
                      </>
                    )}
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
                    {current.kind !== "assume" && (
                      <button className="btn" onClick={() => openSchematic(current.name)} title="Open COI & causal schematic">
                        Schematic
                      </button>
                    )}
                    {hasTrace && (
                      <button
                        className="btn"
                        onClick={() => void launchCexWalkthrough(current.name)}
                        title="Run why_cex and launch a synchronized 3-way guided CEX walkthrough"
                      >
                        ✦ Explain CEX
                      </button>
                    )}
                  </div>
                </div>
                <div className="details-right">
                  <SourceView text={source} focus={focusLine} focusEnd={focusEnd} />
                </div>
              </div>
            ) : (
              <p className="empty">Select a property to see its expression, result and source.</p>
            ))}
          {bottomTab === "messages" && <Messages events={events} />}
          {bottomTab === "info" && (
            <DesignInfo info={info.data} error={info.error} loading={infoLoading} stale={!!info.data && info.of !== session} onRefresh={idle ? loadInfo : undefined} />
          )}
          {bottomTab === "proposals" && ai && <Proposals ai={ai} busy={deciding || !idle} onDecide={decide} />}
          {bottomTab === "profiler" && profiler && <Profiler prop={current} props={allProps} />}
          {bottomTab === "apps" && appsOpen && (
            <FormalApps
              session={session}
              mode={appMode}
              onMode={setAppMode}
              idle={idle}
              onRunTcl={(cmd) => (isProveCmd(cmd) ? prove(cmd) : run(() => backend.evalTcl(cmd), cmd))}
              onSelectTask={(t) => {
                setTaskFilter(t);
                setCenter("table");
              }}
            />
          )}
        </section>
      </div>

      {/* ---- status bar ---- */}
      <footer className="statusbar">
        <span>top: <span className="mono">{session?.design.top ?? "—"}</span></span>
        <span>clock: <span className="mono">{session?.clocks.map((c) => c.signal).join(", ") || "—"}</span></span>
        <span>reset: <span className="mono">{session?.reset ?? "—"}</span></span>
        <span>{flavor === "jg" ? "JasperGold" : "VC Formal"} flavor</span>
        <span className="spacer" />
        {waiting > 0 && (
          <button className="statusbar-btn" title="An agent on the AI interface proposed a change; nothing is applied until you accept" onClick={() => setBottomTab("proposals")}>
            AI: {waiting} proposal{waiting === 1 ? "" : "s"} waiting
          </button>
        )}
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

      <CommandPalette
        open={commandPaletteOpen}
        commands={paletteCommands}
        onClose={() => setCommandPaletteOpen(false)}
      />

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
      {colMenu && (
        <ContextMenu
          x={colMenu.x}
          y={colMenu.y}
          label="Table columns"
          onClose={closeColMenu}
          items={OPTIONAL_COLS.map((c) => ({ label: c.label, checked: !hidden.includes(c.key), title: c.title, onClick: () => toggleCol(c.key) }))}
        />
      )}
      {ctx && ctxProp && <ContextMenu x={ctx.x} y={ctx.y} items={ctxItems} label={`Actions for ${ctxProp.name}`} onClose={closeCtx} />}
    </div>
  );
}
