// Geometric brand mark for InFormal: formal proof turnstile (⊢) morphing into
// a digital step waveform with a verified terminal node.

export function Logo({ size = 18, className = "" }: { size?: number; className?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      className={`brand-logo ${className}`.trim()}
      aria-hidden="true"
    >
      <rect x="1.5" y="1.5" width="29" height="29" rx="7" fill="var(--accent)" />
      <path d="M8.8 9.2V22.8" stroke="#fdfcfa" strokeWidth="2.5" strokeLinecap="round" />
      <path
        d="M8.8 16H14V11.2H19.4V20.8H23.4"
        stroke="#fdfcfa"
        strokeWidth="2.35"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="24.3" cy="20.8" r="1.85" fill="#6ee7b7" />
    </svg>
  );
}
