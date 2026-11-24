import type { LayoutPreset } from "../store/workbenchStore";
import { anyTraceOf, isVacuousProof, traceOf, vacuityTooltip, witnessTraceOf, type Center, type ViewProp } from "../model";
import { Status } from "./Glyph";

export function ContextBar({
  prop,
  center,
  layoutPreset,
  canBack,
  traceCursor,
  hasSource,
  onBack,
  onTable,
  onWaveform,
  onSource,
  onSchematic,
  onCexDebug,
}: {
  prop: ViewProp;
  center: Center;
  layoutPreset: LayoutPreset;
  canBack: boolean;
  traceCursor?: number | null;
  hasSource: boolean;
  onBack: () => void;
  onTable: () => void;
  onWaveform: () => void;
  onSource: () => void;
  onSchematic: () => void;
  onCexDebug: () => void;
}) {
  const hasAnyTrace = Boolean(anyTraceOf(prop));
  const hasCexOrCoverTrace = Boolean(traceOf(prop));
  const isWitnessOnly = Boolean(witnessTraceOf(prop) && !hasCexOrCoverTrace);

  return (
    <div className="context-bar" role="region" aria-label="Active property context">
      <button
        type="button"
        className="btn small context-back"
        onClick={canBack ? onBack : onTable}
        title={canBack ? "Return to previous view and filters (Alt+Left)" : "Return to Property Table (Alt+Left)"}
      >
        ← Back
      </button>
      <span className="context-prop mono strong ellipsis" title={prop.expr}>
        {prop.name}
      </span>
      <Status p={prop} />
      {isVacuousProof(prop) && (
        <span className="tag tag-vacuous" title={vacuityTooltip(prop)} aria-label="vacuous proof">
          ⚠ vacuous
        </span>
      )}
      {traceCursor != null && (
        <span className="context-cycle mono" title="Synchronized waveform cursor cycle">
          @C{traceCursor}
        </span>
      )}
      <span className="spacer" />
      <div className="context-pivots" role="group" aria-label="Switch property view">
        <button
          type="button"
          className={`btn small context-pivot ${layoutPreset === "triage" && center === "table" ? "active" : ""}`}
          aria-pressed={layoutPreset === "triage" && center === "table"}
          onClick={onTable}
        >
          Table
        </button>
        <button
          type="button"
          className={`btn small context-pivot ${layoutPreset === "triage" && center === "trace" ? "active" : ""}`}
          aria-pressed={layoutPreset === "triage" && center === "trace"}
          disabled={!hasAnyTrace}
          title={hasAnyTrace ? `Open ${isWitnessOnly ? "witness" : "waveform"} trace` : "No waveform trace for this property"}
          onClick={onWaveform}
        >
          {isWitnessOnly ? "Witness" : "Waveform"}
        </button>
        <button
          type="button"
          className={`btn small context-pivot ${layoutPreset === "triage" && center === "source" ? "active" : ""}`}
          aria-pressed={layoutPreset === "triage" && center === "source"}
          disabled={!hasSource}
          title="Jump to RTL / Tcl source (F3)"
          onClick={onSource}
        >
          Source
        </button>
        <button
          type="button"
          className={`btn small context-pivot ${layoutPreset === "triage" && center === "schematic" ? "active" : ""}`}
          aria-pressed={layoutPreset === "triage" && center === "schematic"}
          disabled={prop.kind === "assume"}
          title="Open COI & causal schematic"
          onClick={onSchematic}
        >
          Cone
        </button>
        <button
          type="button"
          className={`btn small context-pivot ${layoutPreset === "cex-debug" ? "active" : ""}`}
          aria-pressed={layoutPreset === "cex-debug"}
          disabled={!hasCexOrCoverTrace}
          title="Synchronized 3-pane CEX Debug (Source + Trace + Schematic)"
          onClick={onCexDebug}
        >
          3-Pane CEX
        </button>
      </div>
    </div>
  );
}
