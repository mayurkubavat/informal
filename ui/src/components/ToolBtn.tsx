export function ToolBtn({ title, onClick, disabled, on, children }: { title: string; onClick?: () => void; disabled?: boolean; on?: boolean; children: React.ReactNode }) {
  return (
    <button className={`tool ${on ? "on" : ""}`} title={title} aria-label={title} onClick={onClick} disabled={disabled}>
      {children}
    </button>
  );
}
