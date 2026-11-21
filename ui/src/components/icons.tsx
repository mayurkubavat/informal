// Inline SVG icons (currentColor).

export const ico = {
  width: 16,
  height: 16,
  viewBox: "0 0 16 16",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};
export const PlayIcon = () => <svg {...ico} width={12} height={12} className="play"><path d="M4 2.5v11l9-5.5z" fill="currentColor" stroke="none" /></svg>;
export const StopIcon = () => <svg {...ico}><rect x="4" y="4" width="8" height="8" rx="1" /></svg>;
export const ScriptIcon = () => <svg {...ico}><path d="M4 2h5.5L12.5 5v9H4z" /><path d="M9.5 2v3h3M6 8h4.5M6 10.5h4.5" /></svg>;
export const WaveIcon = () => <svg {...ico}><path d="M1.5 11.5h2.5v-7h3v7h3v-7h2.5v7h2" /></svg>;
export const CodeIcon = () => <svg {...ico}><path d="M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5" /></svg>;
export const SchematicIcon = () => (
  <svg {...ico}>
    <rect x="2" y="3" width="4.5" height="4.5" rx="1" />
    <rect x="2" y="8.5" width="4.5" height="4.5" rx="1" />
    <rect x="9.5" y="5.8" width="4.5" height="4.5" rx="1" />
    <path d="M6.5 5.2H8V8H9.5M6.5 10.8H8V8" />
  </svg>
);
export const ChevronIcon = () => <svg {...ico} width={10} height={10}><path d="M4 6l4 4 4-4" /></svg>;
