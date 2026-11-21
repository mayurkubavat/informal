import { statusKey, statusText, type ViewProp } from "../model";

// Small status glyphs, the only strong colour in the window.
export function Glyph({ k }: { k: string }) {
  const c = `glyph st-${k}`;
  const props = { className: c, width: 12, height: 12, viewBox: "0 0 12 12", "aria-hidden": true, fill: "none", stroke: "currentColor", strokeWidth: 1.6, strokeLinecap: "round" as const, strokeLinejoin: "round" as const };
  switch (k) {
    case "proven":
      return <svg {...props}><path d="M2.5 6.3l2.3 2.3 4.7-5" /></svg>;
    case "bounded_proven":
      return <svg {...props}><circle cx="6" cy="6" r="4.3" strokeDasharray="3 1.5" /><path d="M3.6 6.2l1.7 1.7 3.2-3.4" /></svg>;
    case "cex":
      return <svg {...props}><path d="M3 3l6 6M9 3l-6 6" /></svg>;
    case "covered":
      return <svg {...props}><circle cx="6" cy="6" r="4" /><circle cx="6" cy="6" r="1.5" fill="currentColor" stroke="none" /></svg>;
    case "unreachable":
      return <svg {...props}><circle cx="6" cy="6" r="4" /><path d="M3.2 8.8l5.6-5.6" /></svg>;
    case "undetermined":
      return <svg {...props}><path d="M4.3 4.4a1.8 1.8 0 113 1.4c-.8.5-1.3.8-1.3 1.7" /><circle cx="6" cy="9.6" r=".5" fill="currentColor" /></svg>;
    case "error":
      return <svg {...props}><path d="M6 1.8l4.6 8.2H1.4z" /><path d="M6 5v2.2" /><circle cx="6" cy="8.6" r=".4" fill="currentColor" /></svg>;
    case "running":
      // Dashed ring in the accent colour; CSS spins it unless motion is reduced.
      return <svg {...props}><circle cx="6" cy="6" r="4.2" strokeDasharray="4.6 2" /></svg>;
    case "assume":
      return <svg {...props}><path d="M3 6h6" /></svg>;
    default:
      return <svg {...props}><circle cx="6" cy="6" r="3.5" /></svg>;
  }
}

export function Status({ p }: { p: ViewProp }) {
  return (
    <span className="status">
      <Glyph k={statusKey(p)} />
      {statusText(p)}
    </span>
  );
}
