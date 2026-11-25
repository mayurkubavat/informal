import { useEffect, useRef } from "react";
import { trapFocusKeyDown } from "../focusTrap";

interface GuideGroup {
  title: string;
  subtitle: string;
  items: { keys: string; label: string; tcl?: string }[];
}

const GUIDE_GROUPS: GuideGroup[] = [
  {
    title: "1. Load & Prove",
    subtitle: "Elaborate RTL, pick engine effort, and run multi-pass proofs",
    items: [
      { keys: "F5", label: "Prove all enabled properties in active task", tcl: "prove -all" },
      { keys: "F6", label: "Prove selected assertion or cover", tcl: "prove -property <name>" },
      { keys: "Esc", label: "Close open popover/menu, else stop running prove" },
      { keys: "Effort", label: "Quick · Standard · Deep · Signoff (witness + proof core)", tcl: "set_prove_effort <level>" },
    ],
  },
  {
    title: "2. Triage & Proof Trust",
    subtitle: "Verify non-vacuity, environment consistency, and minimal proof core",
    items: [
      { keys: "↑ / ↓", label: "Move selection in Property Table (Home / End / PgUp / PgDn)" },
      { keys: "Enter", label: "Open CEX / cover waveform, else open Details" },
      { keys: "Shift+F10", label: "Property actions: Prove, Witness, Disable / Enable", tcl: "assert -disable {<name>}" },
      { keys: "Ctrl+F", label: "Filter properties by name, expression, or engine" },
      { keys: "Env pill", label: "Right-click assumptions pill for -conflict / -dead_end / -live", tcl: "check_assumptions -live" },
    ],
  },
  {
    title: "3. Debug CEX, Source & Cone",
    subtitle: "Pivot across waveform, RTL drivers, and COI schematic without losing context",
    items: [
      { keys: "F3", label: "Toggle CodeMirror 6 RTL / Tcl source view" },
      { keys: "Alt+←", label: "Back to previous view, selection, and active table filters" },
      { keys: "← / →", label: "Step waveform cursor 1 cycle (N / P: next / prev transition)" },
      { keys: "Sig menu", label: "Right-click a Design signal to add to trace or cut driver", tcl: "stopat <signal>" },
    ],
  },
  {
    title: "4. Console, Formal Apps & AI",
    subtitle: "Dual-persona Tcl scripting, Formal Apps, and AI Co-Pilot",
    items: [
      { keys: "Ctrl+K", label: "Universal Command Palette (Cmd+K on macOS)" },
      { keys: "Ctrl+L", label: "Focus Tcl / Ask console prompt (Tab cycles completions)" },
      { keys: "Ctrl+1..5", label: "Switch bottom dock: Console · Details · Messages · Log · Profiler" },
      { keys: "Ctrl+Shift+A", label: "Toggle Console between Tcl and AI Ask mode" },
      { keys: "?", label: "Open or close this keyboard & workflow guide" },
    ],
  },
];

export function ShortcutSheet({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const closeBtnRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const prevFocus = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (open) {
      prevFocus.current = document.activeElement as HTMLElement | null;
      closeBtnRef.current?.focus();
      const onGlobalKey = (e: KeyboardEvent) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onCloseRef.current();
        } else if (e.key === "Tab") {
          if (trapFocusKeyDown(dialogRef.current, e)) e.stopPropagation();
        }
      };
      window.addEventListener("keydown", onGlobalKey, true);
      return () => window.removeEventListener("keydown", onGlobalKey, true);
    } else if (prevFocus.current) {
      if (document.contains(prevFocus.current)) prevFocus.current.focus();
      prevFocus.current = null;
    }
  }, [open]);

  if (!open) return null;

  return (
    <div
      className="palette-backdrop shortcut-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="shortcut-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcut-sheet-title"
        onKeyDown={(e) => {
          if (e.key === "Escape" || e.key === "?") {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          }
        }}
      >
        <div className="shortcut-head">
          <div>
            <h2 id="shortcut-sheet-title" className="shortcut-title">
              Keyboard & Formal Workflow Guide
            </h2>
            <p className="shortcut-sub dim small">
              Every UI control echoes its exact JasperGold / VC Formal Tcl equivalent in the Console.
            </p>
          </div>
          <button
            ref={closeBtnRef}
            type="button"
            className="btn small"
            onClick={onClose}
            aria-label="Close keyboard and workflow guide"
          >
            Esc
          </button>
        </div>

        <div className="shortcut-grid">
          {GUIDE_GROUPS.map((g) => (
            <section key={g.title} className="shortcut-section">
              <h3 className="shortcut-h3">{g.title}</h3>
              <p className="shortcut-desc dim small">{g.subtitle}</p>
              <dl className="shortcut-list">
                {g.items.map((it) => (
                  <div key={it.keys + it.label} className="shortcut-row">
                    <dt>
                      <kbd className="palette-kbd mono">{it.keys}</kbd>
                    </dt>
                    <dd>
                      <span>{it.label}</span>
                      {it.tcl && <code className="shortcut-tcl mono">{it.tcl}</code>}
                    </dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
