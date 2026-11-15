// The changes an agent asked for through the AI interface
// (docs/ai-interface.md, decision D17), one card each. A pending one waits
// here for Accept or Reject; nothing reaches the session before that, and
// what is accepted is tagged AI in the property table.
import type { AiProposal, AiStatus } from "../backend";

export const KIND_LABEL: Record<AiProposal["kind"], string> = {
  assume: "Assumption",
  assert: "Assertion",
  cover: "Cover",
  cut_point: "Cut point",
  command: "Tcl command",
};
export const STATE_LABEL: Record<AiProposal["state"], string> = {
  pending: "Waiting for you",
  applied: "Accepted",
  rejected: "Rejected",
  withdrawn: "Withdrawn by the agent",
  undone: "Removed",
  failed: "Could not be applied",
  gone: "Gone with the cleared session",
};

// Pending first (oldest first, the order they were asked in), then the
// decided ones, newest first.
export function ordered(list: AiProposal[]): AiProposal[] {
  const pending = list.filter((p) => p.state === "pending");
  const rest = list.filter((p) => p.state !== "pending").reverse();
  return [...pending, ...rest];
}

export function Proposals({ ai, busy, onDecide }: { ai: AiStatus; busy: boolean; onDecide: (id: number, accept: boolean) => void }) {
  if (!ai.proposals.length) return <p className="empty">No proposals.</p>;
  return (
    <div className="proposals" role="list" aria-label="AI proposals">
      <p className="proposals-note small muted">
        Proposed through the AI interface. Nothing changes until you accept; an accepted change is tagged AI in the property table, and only a prove gives a result.
      </p>
      {ordered(ai.proposals).map((p) => {
        const title = `${KIND_LABEL[p.kind]}${p.name ? ` ${p.name}` : ""}`;
        return (
          <article key={p.id} role="listitem" className={`proposal ${p.state === "pending" ? "" : "decided"}`} aria-label={`Proposal ${p.id}: ${title}`}>
            <header className="proposal-head">
              <span className="strong">{KIND_LABEL[p.kind]}</span>
              {p.name && <span className="mono">{p.name}</span>}
              <span className="spacer" />
              <span className="proposal-state dim">{STATE_LABEL[p.state]}</span>
            </header>
            <pre className="expr">{p.text}</pre>
            <p className="proposal-why">{p.rationale}</p>
            {p.error && <p className="proposal-error mono">{p.error}</p>}
            {p.state === "pending" && (
              <div className="details-actions">
                <button className="btn" disabled={busy} onClick={() => onDecide(p.id, true)} aria-label={`Accept ${title}`}>
                  Accept
                </button>
                <button className="btn" disabled={busy} onClick={() => onDecide(p.id, false)} aria-label={`Reject ${title}`}>
                  Reject
                </button>
              </div>
            )}
          </article>
        );
      })}
    </div>
  );
}
