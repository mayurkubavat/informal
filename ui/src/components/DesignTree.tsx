import { useEffect, useState } from "react";
import type { DesignScope, ScopeSignal } from "../backend";
import { findScope, signalKind } from "../model";
import { ChevronIcon } from "./icons";

// The Design tab, laid out like a simulator's design browser: the instance
// tree on top (twisty to expand, click to select a scope), the selected
// scope's ports and signals below it (name, width, kind) with a filter, and
// the design files at the end. Double-click (or Enter on) a signal to add it
// to the open trace.
export function DesignTree({
  scopes,
  files,
  fallback,
  onAddSignal,
}: {
  scopes: DesignScope[];
  files: string[];
  /** True when the tree is the top file's port scan (no slang hierarchy). */
  fallback?: boolean;
  onAddSignal?: (path: string) => void;
}) {
  const rootPath = scopes[0]?.path ?? null;
  const [sel, setSel] = useState<string | null>(rootPath);
  const [open, setOpen] = useState<Set<string>>(() => new Set(rootPath ? [rootPath] : []));
  const [q, setQ] = useState("");
  const [sig, setSig] = useState<string | null>(null);
  // A new design: select and open its top.
  useEffect(() => {
    setSel(rootPath);
    setOpen(new Set(rootPath ? [rootPath] : []));
  }, [rootPath]);
  if (!scopes.length) return <p className="empty">No design elaborated.</p>;

  const scope = findScope(scopes, sel) ?? scopes[0];
  const needle = q.trim().toLowerCase();
  const signals = scope.signals.filter((s) => !needle || s.name.toLowerCase().includes(needle));
  const toggle = (p: string) =>
    setOpen((o) => {
      const n = new Set(o);
      if (n.has(p)) n.delete(p);
      else n.add(p);
      return n;
    });
  const add = (s: ScopeSignal) => onAddSignal?.(`${scope.path}.${s.name}`);

  const rows: React.ReactNode[] = [];
  const walk = (s: DesignScope, depth: number) => {
    const isOpen = open.has(s.path);
    const kids = s.children.length > 0;
    rows.push(
      <div
        key={s.path}
        className={`tree-row ${s.path === scope.path ? "sel" : ""}`}
        role="treeitem"
        aria-level={depth + 1}
        aria-selected={s.path === scope.path}
        aria-expanded={kids ? isOpen : undefined}
        tabIndex={s.path === scope.path ? 0 : -1}
        style={{ paddingLeft: 6 + depth * 14 }}
        title={`${s.path} (${s.module})`}
        onClick={() => setSel(s.path)}
        onDoubleClick={() => kids && toggle(s.path)}
        onKeyDown={(e) => {
          if (e.key === "ArrowRight" && kids && !isOpen) toggle(s.path);
          else if (e.key === "ArrowLeft" && kids && isOpen) toggle(s.path);
          else return;
          e.preventDefault();
        }}
      >
        <span
          className={`twisty ${isOpen ? "open" : ""} ${kids ? "" : "none"}`}
          onClick={(e) => {
            e.stopPropagation();
            if (kids) toggle(s.path);
          }}
        >
          {kids && <ChevronIcon />}
        </span>
        <span className="mono">{s.name}</span>
        <span className="tree-count mono">{s.module}</span>
      </div>,
    );
    if (isOpen) for (const c of s.children) walk(c, depth + 1);
  };
  for (const s of scopes) walk(s, 0);

  return (
    <div className="design-browser">
      <div className="tree design-scopes" role="tree" aria-label="Design hierarchy">
        {rows}
      </div>
      <div className="design-signals">
        <div className="design-head">
          <span className="mono ellipsis" title={scope.path}>
            {scope.path}
          </span>
          <span className="tree-count">{fallback ? "ports (scan)" : `${scope.signals.length} signals`}</span>
        </div>
        <input className="filter design-filter" placeholder="Filter signals" aria-label="Filter design signals" value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === "Escape" && q && (e.preventDefault(), setQ(""))} />
        <div className="design-siglist" role="listbox" aria-label={`Signals in ${scope.path}`}>
          {signals.map((s) => (
            <div
              key={s.name}
              role="option"
              aria-selected={sig === s.name}
              tabIndex={0}
              className={`tree-row sig-row ${sig === s.name ? "sel" : ""}`}
              title={`${scope.path}.${s.name} · ${s.width || "?"} bit${s.width === 1 ? "" : "s"} · ${s.kind}${s.dir ? ` ${s.dir}` : ""}${onAddSignal ? " (double-click to add to the open trace)" : ""}`}
              onClick={() => setSig(s.name)}
              onDoubleClick={() => add(s)}
              onKeyDown={(e) => e.key === "Enter" && add(s)}
            >
              <span className="mono sig-name">{s.name}</span>
              <span className="mono sig-width">{s.width || "?"}</span>
              <span className="sig-kind">{signalKind(s)}</span>
            </div>
          ))}
          {!signals.length && <p className="empty small">{scope.signals.length ? "No signal matches." : "No signals in this scope."}</p>}
        </div>
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
    </div>
  );
}
