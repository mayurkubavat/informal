// Shared focus-trap utility for `aria-modal="true"` dialogs (CommandPalette,
// ShortcutSheet). Cycles Tab / Shift+Tab explicitly across visible tabbable
// descendants so focus never escapes behind modal backdrops (including in
// macOS WebKit where native Tab skips buttons unless explicitly managed).

const TABBABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  'input:not([disabled]):not([type="hidden"])',
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function isVisibleTabbable(el: HTMLElement): boolean {
  if (el.hasAttribute("hidden") || el.closest("[hidden], [aria-hidden='true']")) return false;
  if (typeof window !== "undefined" && typeof window.getComputedStyle === "function") {
    const style = window.getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden") return false;
  }
  return true;
}

export function getTabbables(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>(TABBABLE_SELECTOR)].filter(isVisibleTabbable);
}

/**
 * Intercepts Tab / Shift+Tab while a modal container is open and cycles focus
 * strictly among its tabbable elements. Returns `true` if handled.
 */
export function trapFocusKeyDown(
  container: HTMLElement | null,
  e: Pick<KeyboardEvent, "key" | "shiftKey" | "preventDefault">,
): boolean {
  if (e.key !== "Tab" || !container) return false;
  const tabbables = getTabbables(container);
  e.preventDefault();
  if (tabbables.length === 0) {
    container.focus?.();
    return true;
  }
  const active = document.activeElement as HTMLElement | null;
  const idx = active ? tabbables.indexOf(active) : -1;
  if (idx < 0) {
    (e.shiftKey ? tabbables[tabbables.length - 1] : tabbables[0]).focus();
    return true;
  }
  const nextIdx = (idx + (e.shiftKey ? -1 : 1) + tabbables.length) % tabbables.length;
  tabbables[nextIdx].focus();
  return true;
}
