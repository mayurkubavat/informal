import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { MenuItem } from "./MenuBar";

// A right-click menu in the same style as the menu bar pop-ups. Opens at
// (x, y), clamped to the window; Escape, an outside click, scrolling or a
// resize close it. Disabled items stay focusable (aria-disabled) so their
// tooltip explains why.
export function ContextMenu({ x, y, items, label, onClose }: { x: number; y: number; items: MenuItem[]; label: string; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  // Hand focus back to where it was (usually the table) when the menu closes.
  // Captured at first render, before the layout effect focuses the first item.
  const [prev] = useState(() => document.activeElement as HTMLElement | null);
  useEffect(() => {
    return () => {
      const a = document.activeElement;
      if (!a || a === document.body || ref.current?.contains(a)) prev?.focus?.();
    };
  }, [prev]);

  useLayoutEffect(() => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return;
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    });
    ref.current?.querySelector<HTMLButtonElement>(".menu-item")?.focus();
  }, [x, y]);

  useEffect(() => {
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && onClose();
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", key, true);
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onClose);
    document.addEventListener("scroll", onClose, true);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onClose);
      document.removeEventListener("scroll", onClose, true);
    };
  }, [onClose]);

  const move = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const all = [...(ref.current?.querySelectorAll<HTMLButtonElement>(".menu-item") ?? [])];
    const i = all.indexOf(document.activeElement as HTMLButtonElement);
    const n =
      e.key === "Home" ? 0 : e.key === "End" ? all.length - 1 : e.key === "ArrowDown" ? (i + 1) % all.length : (i - 1 + all.length) % all.length;
    all[n]?.focus();
  };

  return (
    <div ref={ref} className="menu-pop context-menu" role="menu" aria-label={label} style={pos} onKeyDown={move} onContextMenu={(e) => e.preventDefault()}>
      {items.map((it, i) =>
        "sep" in it ? (
          <div key={i} className="menu-sep" role="separator" />
        ) : (
          <button
            key={i}
            role="menuitem"
            className="menu-item"
            aria-disabled={it.disabled || undefined}
            title={it.title}
            onClick={() => {
              if (it.disabled) return;
              onClose();
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
  );
}
