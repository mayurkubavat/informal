import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Logo } from "./Logo";

export type MenuItem =
  | { label: string; hint?: string; keys?: string; title?: string; onClick?: () => void; disabled?: boolean; checked?: boolean }
  | { sep: true };
export type MenuDef = { label: string; items: MenuItem[] };

export function MenuBar({
  menus,
  macOverlay,
  children,
}: {
  menus: MenuDef[];
  macOverlay?: boolean;
  children?: React.ReactNode;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const bar = useRef<HTMLDivElement>(null);
  const titleRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const popRef = useRef<HTMLDivElement | null>(null);
  const focusPopOnOpen = useRef<"first" | "last" | null>(null);

  const enabledItems = () => [...(popRef.current?.querySelectorAll<HTMLButtonElement>(".menu-item:not(:disabled)") ?? [])];

  useLayoutEffect(() => {
    if (!open || !focusPopOnOpen.current) return;
    const edge = focusPopOnOpen.current;
    focusPopOnOpen.current = null;
    const items = enabledItems();
    if (!items.length) return;
    (edge === "first" ? items[0] : items[items.length - 1]).focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !bar.current?.contains(e.target as Node) && setOpen(null);
    // Mark the Esc as used, so the workbench does not also stop a prove,
    // and return focus to the triggering menubar button.
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      const trigger = titleRefs.current[open];
      setOpen(null);
      trigger?.focus();
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  const moveTopMenu = (curIdx: number, delta: number, openAndFocus: boolean) => {
    if (!menus.length) return;
    const next = menus[(curIdx + delta + menus.length) % menus.length];
    if (openAndFocus) {
      focusPopOnOpen.current = "first";
      setOpen(next.label);
    } else {
      titleRefs.current[next.label]?.focus();
    }
  };

  const onTitleKeyDown = (m: MenuDef, idx: number, e: React.KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === "ArrowRight") {
      e.preventDefault();
      moveTopMenu(idx, 1, open !== null);
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      moveTopMenu(idx, -1, open !== null);
    } else if (e.key === "Home") {
      e.preventDefault();
      if (open !== null) {
        focusPopOnOpen.current = "first";
        setOpen(menus[0].label);
      } else {
        titleRefs.current[menus[0].label]?.focus();
      }
    } else if (e.key === "End") {
      e.preventDefault();
      const last = menus[menus.length - 1];
      if (open !== null) {
        focusPopOnOpen.current = "first";
        setOpen(last.label);
      } else {
        titleRefs.current[last.label]?.focus();
      }
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const edge = e.key === "ArrowDown" ? "first" : "last";
      if (open === m.label) {
        const items = enabledItems();
        if (items.length) (edge === "first" ? items[0] : items[items.length - 1]).focus();
      } else {
        focusPopOnOpen.current = edge;
        setOpen(m.label);
      }
    }
  };

  const onPopKeyDown = (idx: number, e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "ArrowRight") {
      e.preventDefault();
      moveTopMenu(idx, 1, true);
      return;
    }
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      moveTopMenu(idx, -1, true);
      return;
    }
    if (e.key === "Tab") {
      setOpen(null);
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const items = enabledItems();
    if (!items.length) return;
    const cur = items.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      e.key === "Home"
        ? 0
        : e.key === "End"
          ? items.length - 1
          : e.key === "ArrowDown"
            ? (cur + 1) % items.length
            : (cur - 1 + items.length) % items.length;
    items[next]?.focus();
  };

  const dragProps = macOverlay ? { "data-tauri-drag-region": "" } : {};
  return (
    <div className={`menubar ${macOverlay ? "mac-overlay" : ""}`} ref={bar} {...dragProps}>
      <span className="menubar-brand" title="InFormal Formal Verification" {...dragProps}>
        <Logo size={16} />
      </span>
      {/* role lives on a layout-neutral wrapper: a menubar may only own menu items, not the script path field */}
      <div role="menubar" aria-label="Main menu" style={{ display: "contents" }}>
      {menus.map((m, idx) => (
        <div key={m.label} className="menu">
          <button
            ref={(el) => {
              titleRefs.current[m.label] = el;
            }}
            className={`menu-title ${open === m.label ? "on" : ""}`}
            role="menuitem"
            aria-haspopup="true"
            aria-expanded={open === m.label}
            onClick={() => setOpen((o) => (o === m.label ? null : m.label))}
            onMouseEnter={() => open && setOpen(m.label)}
            onKeyDown={(e) => onTitleKeyDown(m, idx, e)}
          >
            {m.label}
          </button>
          {open === m.label && (
            <div ref={popRef} className="menu-pop" role="menu" aria-label={m.label} onKeyDown={(e) => onPopKeyDown(idx, e)}>
              {m.items.map((it, i) =>
                "sep" in it ? (
                  <div key={i} className="menu-sep" role="separator" />
                ) : (
                  <button
                    key={i}
                    role="menuitem"
                    className="menu-item"
                    disabled={it.disabled}
                    title={it.title}
                    onClick={() => {
                      setOpen(null);
                      it.onClick?.();
                    }}
                  >
                    <span className="menu-check">{it.checked ? "✓" : ""}</span>
                    <span>{it.label}</span>
                    {it.hint && <span className="menu-hint mono">{it.hint}</span>}
                    {it.keys && <span className="menu-keys">{it.keys}</span>}
                  </button>
                ),
              )}
            </div>
          )}
        </div>
      ))}
      </div>
      <span className="spacer" {...dragProps} />
      {children}
    </div>
  );
}
