import { useEffect, useMemo, useRef } from "react";
import type { CovItemView } from "../backend";

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Source with line numbers; only the property's own line is highlighted and
// scrolled to the middle of the pane (the pane only, never the page). With
// `focusEnd`, the lines from `focus` to it are highlighted (a range an
// agent pointed at). Optional `causalSignals` (`why_cex` active slice) and
// `covItems` (`check_cov` line/branch status) render subtle gutter markers.
export function SourceView({
  text,
  focus,
  focusEnd,
  causalSignals,
  covItems,
}: {
  text: string;
  focus: number;
  focusEnd?: number;
  causalSignals?: string[];
  covItems?: CovItemView[];
}) {
  const preRef = useRef<HTMLPreElement>(null);
  const focusRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const pre = preRef.current;
    const el = focusRef.current;
    if (!pre || !el) return;
    const top = el.offsetTop - pre.offsetTop - (pre.clientHeight - el.offsetHeight) / 2;
    pre.scrollTop = Math.max(0, top);
  }, [focus, text]);

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
      if (it.status === "unreachable") {
        m.set(it.line, "unreachable");
      } else if (it.status === "covered" && cur !== "unreachable") {
        m.set(it.line, "covered");
      } else if (it.status === "waived" && !cur) {
        m.set(it.line, "waived");
      }
    }
    return m;
  }, [covItems]);

  const last = Math.max(focus, focusEnd ?? focus);
  if (!text) return <p className="empty">No source loaded.</p>;
  return (
    <pre className="source" ref={preRef} data-scroll-quiet data-focus-line={focus || undefined}>
      {text.split("\n").map((line, i) => {
        const lno = i + 1;
        const isFocus = focus > 0 && lno >= focus && lno <= last;
        const isCausal = !!causalLeafRe && causalLeafRe.test(line);
        const cov = covByLine.get(lno);
        const cls = [isFocus ? "focus" : "", isCausal ? "causal-line" : ""].filter(Boolean).join(" ");
        return (
          <div
            key={i}
            ref={lno === focus ? focusRef : undefined}
            className={cls}
            data-causal={isCausal ? "true" : undefined}
            data-cov={cov}
          >
            <span className="ln">
              {cov && (
                <span
                  className={`cov-gutter cov-${cov}`}
                  title={cov === "unreachable" ? "UNR: unreachable in formal model" : `Coverage: ${cov}`}
                  aria-label={`Line ${lno} coverage ${cov}`}
                >
                  {cov === "unreachable" ? "⊘" : "●"}
                </span>
              )}
              {lno}
            </span>
            {line || " "}
          </div>
        );
      })}
    </pre>
  );
}
