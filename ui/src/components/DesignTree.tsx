import { useState } from "react";
import type { Port } from "../model";
import { ChevronIcon } from "./icons";

export function DesignTree({ top, files, ports }: { top: string | null; files: string[]; ports: Port[] }) {
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
