// Guided AI Root-Cause Walkthrough storyboard bar (`gui_present_walkthrough`)
// & Follow-Agent status pill. Steps synchronize Waveform cursor, AI callouts,
// CodeMirror RTL source highlights, and Schematic focus.

import type { WalkthroughState, WalkthroughStep } from "../store/workbenchStore";

export function WalkthroughBar({
  walkthrough,
  followAgent,
  onStep,
  onToggleFollow,
  onClose,
}: {
  walkthrough: WalkthroughState;
  followAgent: boolean;
  onStep: (nextIndex: number, step: WalkthroughStep) => void;
  onToggleFollow: () => void;
  onClose: () => void;
}) {
  const { title, steps, step } = walkthrough;
  const current = steps[step];
  if (!current) return null;

  return (
    <div className="walkthrough-bar" role="region" aria-label={`Guided walkthrough: ${title}`}>
      <span className="walkthrough-badge">✦ AI Tour</span>
      <span className="strong">{title}:</span>
      <button
        className="btn small"
        disabled={step <= 0}
        onClick={() => onStep(step - 1, steps[step - 1])}
        aria-label="Previous walkthrough step"
      >
        ← Prev
      </button>
      <span className="mono small walkthrough-counter">
        {step + 1} / {steps.length}
      </span>
      <button
        className="btn small"
        disabled={step >= steps.length - 1}
        onClick={() => onStep(step + 1, steps[step + 1])}
        aria-label="Next walkthrough step"
      >
        Next →
      </button>
      <span className="walkthrough-note ellipsis" title={`${current.label} — ${current.note}`}>
        <strong>{current.label}:</strong> {current.note}
      </span>
      <span className="spacer" />
      <button
        className={`btn small follow-pill ${followAgent ? "following" : "paused"}`}
        onClick={onToggleFollow}
        aria-pressed={followAgent}
        title={followAgent ? "Auto-panning views with AI steps (click to pause)" : "Manual navigation paused auto-pan (click to resume following AI)"}
      >
        {followAgent ? "● Following AI" : "⏸ Paused · Resume"}
      </button>
      <button className="btn small" onClick={onClose} aria-label="Dismiss walkthrough">
        ×
      </button>
    </div>
  );
}
