// Universal `Cmd+K` / `Ctrl+K` fuzzy Command Palette sharing the unified
// action handlers with `gui_*` screen-control tools.

import { useEffect, useMemo, useRef, useState } from "react";

export interface PaletteCommand {
  id: string;
  group: string;
  label: string;
  shortcut?: string;
  detail?: string;
  disabled?: boolean;
  run: () => void;
}

export function CommandPalette({
  open,
  commands,
  onClose,
}: {
  open: boolean;
  commands: PaletteCommand[];
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [activeIdx, setActiveIdx] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const prevFocus = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (open) {
      prevFocus.current = document.activeElement as HTMLElement | null;
      setQuery("");
      setActiveIdx(0);
      setTimeout(() => inputRef.current?.focus(), 0);
    } else if (prevFocus.current) {
      prevFocus.current.focus();
      prevFocus.current = null;
    }
  }, [open]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const list = commands.filter((c) => !c.disabled);
    if (!q) return list;
    const tokens = q.split(/\s+/);
    return list.filter((c) => {
      const hay = `${c.group} ${c.label} ${c.detail ?? ""}`.toLowerCase();
      return tokens.every((t) => hay.includes(t));
    });
  }, [commands, query]);

  useEffect(() => {
    if (activeIdx >= filtered.length) setActiveIdx(0);
  }, [filtered.length, activeIdx]);

  if (!open) return null;

  const execute = (cmd: PaletteCommand | undefined) => {
    if (!cmd || cmd.disabled) return;
    onClose();
    cmd.run();
  };

  return (
    <div
      className="palette-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="palette-modal"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            setActiveIdx((i) => (filtered.length ? (i + 1) % filtered.length : 0));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActiveIdx((i) => (filtered.length ? (i - 1 + filtered.length) % filtered.length : 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            execute(filtered[activeIdx]);
          }
        }}
      >
        <div className="palette-head">
          <input
            ref={inputRef}
            className="palette-input"
            type="text"
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-listbox"
            aria-activedescendant={filtered[activeIdx] ? `cmd-${filtered[activeIdx].id}` : undefined}
            aria-label="Search commands and properties"
            placeholder="Type a command, property, view, layout preset, or Formal App…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActiveIdx(0);
            }}
          />
          <span className="muted small mono">Esc</span>
        </div>

        <div id="palette-listbox" className="palette-list" role="listbox" aria-label="Matching commands">
          {filtered.length === 0 ? (
            <p className="empty small">No matching commands.</p>
          ) : (
            filtered.map((cmd, idx) => (
              <div
                key={cmd.id}
                id={`cmd-${cmd.id}`}
                role="option"
                aria-selected={idx === activeIdx}
                className={`palette-item ${idx === activeIdx ? "active" : ""}`}
                onMouseEnter={() => setActiveIdx(idx)}
                onClick={() => execute(cmd)}
              >
                <span className="palette-group">{cmd.group}</span>
                <span className="palette-label">{cmd.label}</span>
                {cmd.detail && <span className="muted small mono ellipsis">{cmd.detail}</span>}
                <span className="spacer" />
                {cmd.shortcut && <kbd className="palette-kbd mono">{cmd.shortcut}</kbd>}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
