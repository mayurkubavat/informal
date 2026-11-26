// CodeMirror 6 RTL & Tcl Source Viewer with:
// - SystemVerilog / SVA syntax highlighting
// - Live Active Trace Value inline annotations (`// q = 4'h9 @ C11`) synced to TraceView cursor
// - Live `svlang lint` (`formal-ready` `SVA-FPV-001..005`) diagnostic squiggles & 1-click Quick-Fixes
// - Go-to-Driver / Go-to-Load (`signal_nav`) cross-hierarchy navigation popover
// - `why_cex` active causal line tint and `check_cov` UNR/covered gutter markers

import { useEffect, useMemo, useRef, useState } from "react";
import { Compartment, EditorState, RangeSetBuilder } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, GutterMarker, gutter, lineNumbers, ViewPlugin, type ViewUpdate, WidgetType } from "@codemirror/view";
import { backend, type CovItemView, type FormalLintFindingView, type FormalQuickFixView, type SignalNavView, type SignalRefView } from "../backend";
import { useWorkbenchStore } from "../store/workbenchStore";

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

const SV_KEYWORDS = new Set([
  "module", "endmodule", "input", "output", "inout", "logic", "wire", "reg", "bit", "parameter", "localparam",
  "always_ff", "always_comb", "always", "assign", "begin", "end", "if", "else", "case", "endcase", "default",
  "posedge", "negedge", "or", "and", "assert", "assume", "cover", "property", "sequence", "disable", "iff",
  "clock", "reset", "prove", "elaborate", "analyze", "clear",
]);

class FocusLinePrefixWidget extends WidgetType {
  constructor(readonly lineNo: number) {
    super();
  }
  eq(other: FocusLinePrefixWidget): boolean {
    return this.lineNo === other.lineNo;
  }
  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "cm-sr-ln";
    span.setAttribute("aria-hidden", "true");
    span.textContent = String(this.lineNo);
    return span;
  }
}

class TraceInlineValueWidget extends WidgetType {
  constructor(readonly summary: string) {
    super();
  }
  eq(other: TraceInlineValueWidget): boolean {
    return this.summary === other.summary;
  }
  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "cm-trace-inline mono";
    span.setAttribute("data-testid", "cm-trace-inline");
    span.textContent = `  /* ${this.summary} */`;
    return span;
  }
}

class CovGutterMarker extends GutterMarker {
  constructor(readonly status: "covered" | "unreachable" | "waived", readonly lineNo: number) {
    super();
  }
  eq(other: CovGutterMarker): boolean {
    return this.status === other.status && this.lineNo === other.lineNo;
  }
  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = `cov-gutter cov-${this.status}`;
    span.title = this.status === "unreachable" ? "UNR: unreachable in formal model" : `Coverage: ${this.status}`;
    span.setAttribute("aria-label", `Line ${this.lineNo} coverage ${this.status}`);
    span.textContent = this.status === "unreachable" ? "⊘" : "●";
    return span;
  }
}

function buildSvSyntaxPlugin() {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = this.compute(view);
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged) this.decorations = this.compute(u.view);
      }
      compute(view: EditorView): DecorationSet {
        const builder = new RangeSetBuilder<Decoration>();
        const doc = view.state.doc;
        const from = view.viewport.from;
        const to = Math.min(doc.length, view.viewport.to || doc.length);
        const slice = doc.sliceString(from, to);
        const tokenRe = /\/\/[^\n]*|\b\d+'[bhdBHD][0-9a-fA-FxXzZ_]+\b|\$[a-zA-Z_]\w*|\b[a-zA-Z_]\w*\b/g;
        let m: RegExpExecArray | null;
        while ((m = tokenRe.exec(slice)) !== null) {
          const tok = m[0];
          const start = from + m.index;
          const end = start + tok.length;
          if (tok.startsWith("//")) {
            builder.add(start, end, Decoration.mark({ class: "cm-sv-comment" }));
          } else if (tok.startsWith("$")) {
            builder.add(start, end, Decoration.mark({ class: "cm-sv-sys" }));
          } else if (/^\d+'/.test(tok)) {
            builder.add(start, end, Decoration.mark({ class: "cm-sv-lit" }));
          } else if (SV_KEYWORDS.has(tok)) {
            builder.add(start, end, Decoration.mark({ class: "cm-sv-kw" }));
          }
        }
        return builder.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
}

export function SourceView({
  text,
  file,
  switchFiles,
  onSelectFile,
  focus,
  focusEnd,
  causalSignals,
  covItems,
  lintFindings,
  onApplyFix,
  onJumpLocation,
  onPinContext,
}: {
  text: string;
  file?: string | null;
  switchFiles?: string[];
  onSelectFile?: (file: string) => void;
  focus: number;
  focusEnd?: number;
  causalSignals?: string[];
  covItems?: CovItemView[];
  lintFindings?: FormalLintFindingView[];
  onApplyFix?: (fix: FormalQuickFixView) => void;
  onJumpLocation?: (file: string, line: number) => void;
  onPinContext?: (label: string, detail: string) => void;
}) {
  const hostRef = useRef<HTMLPreElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const overlayComp = useRef(new Compartment());

  const activeTraceCycle = useWorkbenchStore((st) => st.activeTraceCycle);
  const activeTraceValues = useWorkbenchStore((st) => st.activeTraceValues);

  const [selectedWord, setSelectedWord] = useState<string | null>(null);
  const [navResult, setNavResult] = useState<SignalNavView | null>(null);
  const [navLoading, setNavLoading] = useState(false);

  const causalLeafRe = useMemo(() => {
    if (!causalSignals?.length) return null;
    const leaves = Array.from(
      new Set(
        causalSignals
          .map((s) => s.split(".").pop()?.trim() ?? "")
          .filter((s) => s.length > 0),
      ),
    );
    if (!leaves.length) return null;
    return new RegExp(`\\b(${leaves.map(escapeRegExp).join("|")})\\b`);
  }, [causalSignals]);

  const covByLine = useMemo(() => {
    const m = new Map<number, "covered" | "unreachable" | "waived">();
    for (const it of covItems ?? []) {
      if (!it.line) continue;
      const cur = m.get(it.line);
      if (it.status === "unreachable") m.set(it.line, "unreachable");
      else if (it.status === "covered" && cur !== "unreachable") m.set(it.line, "covered");
      else if (it.status === "waived" && !cur) m.set(it.line, "waived");
    }
    return m;
  }, [covItems]);

  const lintByLine = useMemo(() => {
    const m = new Map<number, FormalLintFindingView[]>();
    for (const f of lintFindings ?? []) {
      const list = m.get(f.line) ?? [];
      list.push(f);
      m.set(f.line, list);
    }
    return m;
  }, [lintFindings]);

  const buildOverlayExtension = useMemo(() => {
    const last = Math.max(focus, focusEnd ?? focus);
    const traceKeys = Object.keys(activeTraceValues).filter((k) => !k.includes(".") && k !== "clk");
    const traceRe = traceKeys.length ? new RegExp(`\\b(${traceKeys.map(escapeRegExp).join("|")})\\b`, "g") : null;

    const decos = EditorView.decorations.compute(["doc"], (state) => {
      const builder = new RangeSetBuilder<Decoration>();
      const doc = state.doc;
      for (let lno = 1; lno <= doc.lines; lno++) {
        const line = doc.line(lno);
        const lineText = line.text;
        const codePart = lineText.split("//")[0] ?? lineText;

        const isFocus = focus > 0 && lno >= focus && lno <= last;
        const isCausal = !!causalLeafRe && causalLeafRe.test(codePart);
        const cov = covByLine.get(lno);
        const diags = lintByLine.get(lno);

        const classes: string[] = [];
        if (isFocus) classes.push("focus");
        if (isCausal) classes.push("causal-line");
        if (diags?.length) {
          const sev = diags[0].severity;
          classes.push(sev === "error" ? "cm-lint-line-error" : sev === "warning" ? "cm-lint-line-warn" : "cm-lint-line-info");
        }
        const attrs: Record<string, string> = {};
        if (isCausal) attrs["data-causal"] = "true";
        if (cov) attrs["data-cov"] = cov;
        if (diags?.length) attrs["data-lint-rule"] = diags.map((d) => d.rule).join(",");

        if (classes.length || Object.keys(attrs).length) {
          builder.add(
            line.from,
            line.from,
            Decoration.line({
              class: classes.join(" "),
              attributes: Object.keys(attrs).length ? attrs : undefined,
            }),
          );
        }

        if (isFocus) {
          builder.add(line.from, line.from, Decoration.widget({ widget: new FocusLinePrefixWidget(lno), side: -1 }));
        }

        // Inline Active Trace Value decoration at end of RTL (.sv/.svh/.v) lines when trace is open
        const isRtl = Boolean(file && /\.(sv|svh|v)$/i.test(file));
        if (isRtl && activeTraceCycle !== null && traceRe && codePart.trim().length > 0 && !codePart.trimStart().startsWith("module ")) {
          const seen = new Set<string>();
          let m: RegExpExecArray | null;
          traceRe.lastIndex = 0;
          while ((m = traceRe.exec(codePart)) !== null) {
            const sig = m[1];
            if (activeTraceValues[sig]) seen.add(`${sig}=${activeTraceValues[sig]}`);
            if (seen.size >= 4) break;
          }
          if (seen.size > 0) {
            const summary = `${Array.from(seen).join(", ")} @C${activeTraceCycle}`;
            builder.add(line.to, line.to, Decoration.widget({ widget: new TraceInlineValueWidget(summary), side: 1 }));
          }
        }
      }
      return builder.finish();
    });

    const covGutter = covByLine.size
      ? gutter({
          class: "cm-cov-gutter",
          lineMarker(view, line) {
            const lno = view.state.doc.lineAt(line.from).number;
            const st = covByLine.get(lno);
            return st ? new CovGutterMarker(st, lno) : null;
          },
        })
      : [];

    return [covGutter, decos];
  }, [file, focus, focusEnd, causalLeafRe, covByLine, lintByLine, activeTraceCycle, activeTraceValues]);

  // Mount / update CodeMirror 6 instance
  useEffect(() => {
    const parent = hostRef.current;
    if (!parent || !text) return;

    const existing = viewRef.current;
    if (!existing) {
      const state = EditorState.create({
        doc: text,
        extensions: [
          EditorState.readOnly.of(true),
          EditorView.editable.of(false),
          EditorView.contentAttributes.of({ tabindex: "0", "aria-label": file ? `Source of ${file}` : "Source view" }),
          lineNumbers(),
          buildSvSyntaxPlugin(),
          overlayComp.current.of(buildOverlayExtension),
          EditorView.domEventHandlers({
            dblclick: (_ev, view) => {
              const pos = view.state.selection.main.head;
              const line = view.state.doc.lineAt(pos);
              const rel = pos - line.from;
              const left = line.text.slice(0, rel).match(/[a-zA-Z_]\w*$/)?.[0] ?? "";
              const right = line.text.slice(rel).match(/^[a-zA-Z0-9_]*/)?.[0] ?? "";
              const word = (left + right).trim();
              if (word && !SV_KEYWORDS.has(word)) {
                inspectSignal(word);
              }
            },
          }),
        ],
      });
      const v = new EditorView({ state, parent });
      viewRef.current = v;
    } else {
      const curDoc = existing.state.doc.toString();
      if (curDoc !== text) {
        existing.dispatch({
          changes: { from: 0, to: curDoc.length, insert: text },
          effects: overlayComp.current.reconfigure(buildOverlayExtension),
        });
      } else {
        existing.dispatch({
          effects: overlayComp.current.reconfigure(buildOverlayExtension),
        });
      }
    }
  }, [text, file, buildOverlayExtension]);

  // Destroy on unmount or when text becomes empty
  useEffect(() => {
    if (!text && viewRef.current) {
      viewRef.current.destroy();
      viewRef.current = null;
    }
  }, [text]);

  useEffect(() => {
    return () => {
      viewRef.current?.destroy();
      viewRef.current = null;
    };
  }, []);

  // Scroll focused line into center of CodeMirror scroller (pane only, never window)
  useEffect(() => {
    const v = viewRef.current;
    if (!v || focus <= 0 || focus > v.state.doc.lines) return;
    const line = v.state.doc.line(focus);
    try {
      v.dispatch({
        effects: EditorView.scrollIntoView(line.from, { y: "center" }),
      });
    } catch {
      // jsdom layout fallback
    }
  }, [focus, text]);

  const inspectSignal = (sig: string) => {
    setSelectedWord(sig);
    setNavLoading(true);
    backend
      .signalNav(sig)
      .then((res) => setNavResult(res))
      .catch(() => setNavResult(null))
      .finally(() => setNavLoading(false));
  };

  if (!text) return <p className="empty">No source loaded.</p>;

  const hasLint = (lintFindings?.length ?? 0) > 0;
  const hasSwitcher = Boolean(switchFiles && switchFiles.length > 1 && onSelectFile);
  const isRtlFile = Boolean(file && /\.(sv|svh|v)$/i.test(file));
  const showToolbar = Boolean(file || hasLint || selectedWord || hasSwitcher);

  return (
    <div className="source-wrap">
      {showToolbar && (
        <div className="source-bar">
          {hasSwitcher ? (
            <div className="source-file-switcher" role="group" aria-label="Source files">
              {switchFiles!.map((f) => {
                const base = f.split("/").pop() ?? f;
                const active = f === file;
                return (
                  <button
                    key={f}
                    type="button"
                    className={`source-file-pill mono ${active ? "on" : ""}`}
                    aria-pressed={active}
                    title={f}
                    onClick={() => onSelectFile!(f)}
                  >
                    {base}
                  </button>
                );
              })}
            </div>
          ) : (
            file && <span className="source-file-label mono small muted ellipsis" title={file}>{file}</span>
          )}
          {isRtlFile && activeTraceCycle !== null && (
            <span className="source-live-badge mono" title="Inline RTL signal values are live-synced to the open waveform cursor">
              ● Live values @C{activeTraceCycle}
            </span>
          )}
          <span className="spacer" />
          {file && (
            <input
              className="filter source-sig-input mono"
              placeholder="Jump to driver/load (signal)…"
              aria-label="Go to signal driver or load"
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  const v = e.currentTarget.value.trim();
                  if (v) inspectSignal(v);
                }
              }}
            />
          )}
          {file && focus > 0 && onPinContext && (
            <button
              className="btn small"
              onClick={() => onPinContext(`@${file.split("/").pop()}:L${focus}`, `Source ${file} line ${focus}`)}
              title="Attach source location as @context pill for AI"
            >
              ✦ @line
            </button>
          )}
          {file && backend.inMcpApp && (
            <button
              className="btn small"
              onClick={() => void backend.openHostEditorFile(file)}
              title="Open this RTL file in Antigravity's editor pane"
            >
              Open in Editor ↗
            </button>
          )}
        </div>
      )}

      {hasLint && (
        <div className="lint-strip" role="region" aria-label="Formal-readiness lint diagnostics">
          {lintFindings!.slice(0, 4).map((d, i) => (
            <div key={`${d.rule}-${d.line}-${i}`} className={`lint-item lint-${d.severity}`}>
              <span className="lint-rule mono strong">{d.rule}</span>
              <span className="lint-line muted mono">L{d.line}</span>
              <span className="lint-msg ellipsis" title={d.message}>{d.message}</span>
              {d.fixes.map((fx, fi) => (
                <button
                  key={fi}
                  className="btn small lint-fix-btn"
                  onClick={() => onApplyFix?.(fx)}
                  title={`${fx.description} (${fx.safety})`}
                >
                  Quick-Fix: {fx.description}
                </button>
              ))}
            </div>
          ))}
        </div>
      )}

      {selectedWord && (
        <div className="nav-popover" role="region" aria-label={`Navigation for ${selectedWord}`}>
          <div className="nav-popover-head">
            <span className="mono strong">Signal: {selectedWord}</span>
            {activeTraceValues[selectedWord] && (
              <span className="mono small accent-pill">{activeTraceValues[selectedWord]}</span>
            )}
            <span className="spacer" />
            {onPinContext && (
              <button
                className="btn small"
                onClick={() =>
                  onPinContext(
                    `@${selectedWord}`,
                    `Signal ${selectedWord}${activeTraceValues[selectedWord] ? ` = ${activeTraceValues[selectedWord]}` : ""}`,
                  )
                }
              >
                ✦ Pin
              </button>
            )}
            <button className="btn small" onClick={() => { setSelectedWord(null); setNavResult(null); }} aria-label="Close signal navigation">
              ×
            </button>
          </div>
          {navLoading ? (
            <p className="small muted">Resolving drivers and loads…</p>
          ) : !navResult ? (
            <p className="small muted">No references found.</p>
          ) : (
            <div className="nav-cols">
              <NavGroup title="Drivers" refs={navResult.drivers} onJump={onJumpLocation} />
              <NavGroup title="Declarations" refs={navResult.declarations} onJump={onJumpLocation} />
              <NavGroup title="Loads" refs={navResult.loads} onJump={onJumpLocation} />
            </div>
          )}
        </div>
      )}

      <pre
        className="source cm-source-host"
        ref={hostRef}
        data-scroll-quiet
        data-focus-line={focus || undefined}
      />
    </div>
  );
}

function NavGroup({
  title,
  refs,
  onJump,
}: {
  title: string;
  refs: SignalRefView[];
  onJump?: (file: string, line: number) => void;
}) {
  return (
    <div className="nav-group">
      <div className="small strong muted">{title} ({refs.length})</div>
      {refs.length === 0 ? (
        <div className="small muted">—</div>
      ) : (
        refs.slice(0, 5).map((r, i) => (
          <button
            key={i}
            className="nav-ref mono small"
            onClick={() => onJump?.(r.file, r.line)}
            title={`${r.file}:${r.line} (${r.kind})`}
          >
            <span className="nav-loc">{r.file.split("/").pop()}:{r.line}</span>
            <span className="muted">[{r.kind}]</span>
            <span className="ellipsis">{r.snippet}</span>
          </button>
        ))
      )}
    </div>
  );
}
