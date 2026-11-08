import { useEffect, useRef, useState } from "react";

export type MenuItem =
  | { label: string; hint?: string; title?: string; onClick?: () => void; disabled?: boolean; checked?: boolean }
  | { sep: true };
export type MenuDef = { label: string; items: MenuItem[] };

export function MenuBar({ menus, children }: { menus: MenuDef[]; children?: React.ReactNode }) {
  const [open, setOpen] = useState<string | null>(null);
  const bar = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !bar.current?.contains(e.target as Node) && setOpen(null);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(null);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  return (
    <div className="menubar" ref={bar}>
      {/* role lives on a layout-neutral wrapper: a menubar may only own menu items, not the script path field */}
      <div role="menubar" aria-label="Main menu" style={{ display: "contents" }}>
      {menus.map((m) => (
        <div key={m.label} className="menu">
          <button
            className={`menu-title ${open === m.label ? "on" : ""}`}
            role="menuitem"
            aria-haspopup="true"
            aria-expanded={open === m.label}
            onClick={() => setOpen((o) => (o === m.label ? null : m.label))}
            onMouseEnter={() => open && setOpen(m.label)}
          >
            {m.label}
          </button>
          {open === m.label && (
            <div className="menu-pop" role="menu">
              {m.items.map((it, i) =>
                "sep" in it ? (
                  <div key={i} className="menu-sep" />
                ) : (
                  <button
                    key={i}
                    role="menuitem"
                    className="menu-item"
                    disabled={it.disabled}
                    onClick={() => {
                      setOpen(null);
                      it.onClick?.();
                    }}
                  >
                    <span className="menu-check">{it.checked ? "✓" : ""}</span>
                    <span>{it.label}</span>
                    {it.hint && <span className="menu-hint mono">{it.hint}</span>}
                  </button>
                ),
              )}
            </div>
          )}
        </div>
      ))}
      </div>
      <span className="spacer" />
      {children}
    </div>
  );
}
