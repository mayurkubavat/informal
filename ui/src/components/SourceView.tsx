import { useEffect, useRef } from "react";

// Source with line numbers; only the property's own line is highlighted.
export function SourceView({ text, focus }: { text: string; focus: number }) {
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
