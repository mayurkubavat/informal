// A tab strip. A tab given an `onClose` (third element) is a document tab:
// it gets a close button and closes on middle-click or Delete. The close
// buttons sit outside the tablist (only tabs may be its children) and are
// placed next to their tab with flex `order`.
export type TabDef = [key: string, label: string, onClose?: () => void];

// `extra` sits at the far right of the strip (the console's input-mode switch).
export function Tabs({ tabs, value, onChange, extra }: { tabs: TabDef[]; value: string; onChange: (v: string) => void; extra?: React.ReactNode }) {
  return (
    <div className="tabs">
      <div role="tablist" className="tablist">
        {tabs.map(([k, label, onClose], i) => (
          <button
            key={k}
            role="tab"
            aria-selected={value === k}
            className={`tab ${value === k ? "on" : ""} ${onClose ? "tab-doc" : ""}`}
            style={{ order: 2 * i }}
            onClick={() => onChange(k)}
            onAuxClick={onClose && ((e) => e.button === 1 && onClose())}
            onKeyDown={onClose && ((e) => e.key === "Delete" && onClose())}
          >
            {label}
          </button>
        ))}
      </div>
      {tabs.map(
        ([k, label, onClose], i) =>
          onClose && (
            <button
              key={k}
              className={`tab-close ${value === k ? "on" : ""}`}
              style={{ order: 2 * i + 1 }}
              aria-label={`Close ${label}`}
              title={`Close ${label}`}
              onClick={onClose}
            >
              ×
            </button>
          ),
      )}
      {extra && <div className="tabs-extra" style={{ order: 2 * tabs.length + 1 }}>{extra}</div>}
    </div>
  );
}
