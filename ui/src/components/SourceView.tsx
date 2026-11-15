import { useEffect, useRef } from "react";

// Source with line numbers; only the property's own line is highlighted and
// scrolled to the middle of the pane (the pane only, never the page). With
// `focusEnd`, the lines from `focus` to it are highlighted (a range an
// agent pointed at).
export function SourceView({ text, focus, focusEnd }: { text: string; focus: number; focusEnd?: number }) {
  const preRef = useRef<HTMLPreElement>(null);
  const focusRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const pre = preRef.current;
    const el = focusRef.current;
    if (!pre || !el) return;
    const top = el.offsetTop - pre.offsetTop - (pre.clientHeight - el.offsetHeight) / 2;
    pre.scrollTop = Math.max(0, top);
  }, [focus, text]);
  const last = Math.max(focus, focusEnd ?? focus);
  if (!text) return <p className="empty">No source loaded.</p>;
  return (
    <pre className="source" ref={preRef} data-scroll-quiet data-focus-line={focus || undefined}>
      {text.split("\n").map((line, i) => (
        <div key={i} ref={i + 1 === focus ? focusRef : undefined} className={focus > 0 && i + 1 >= focus && i + 1 <= last ? "focus" : ""}>
          <span className="ln">{i + 1}</span>
          {line || " "}
        </div>
      ))}
    </pre>
  );
}
