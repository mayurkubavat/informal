import { describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ordered, Proposals } from "./Proposals";
import type { AiProposal, AiStatus } from "../backend";

const p = (id: number, state: AiProposal["state"], over: Partial<AiProposal> = {}): AiProposal => ({
  id,
  kind: "assume",
  name: `m_${id}`,
  text: "en |=> en",
  rationale: `why ${id}`,
  state,
  ...over,
});
const ai = (proposals: AiProposal[]): AiStatus => ({ mode: "propose", socket: null, error: null, proposals });

describe("Proposals", () => {
  it("puts what waits first, then the decided ones, newest first", () => {
    const list = [p(1, "applied"), p(2, "pending"), p(3, "rejected"), p(4, "pending")];
    expect(ordered(list).map((q) => q.id)).toEqual([2, 4, 3, 1]);
  });
  it("shows a pending proposal as a card with its text, the reason and both answers", async () => {
    const onDecide = vi.fn();
    render(<Proposals ai={ai([p(1, "pending"), p(2, "pending", { kind: "cut_point", name: undefined, text: "u_core.state" })])} busy={false} onDecide={onDecide} />);
    const cards = screen.getAllByRole("listitem");
    expect(cards).toHaveLength(2);
    const first = within(cards[0]);
    expect(cards[0]).toHaveAccessibleName("Proposal 1: Assumption m_1");
    expect(first.getByText("en |=> en")).toBeInTheDocument();
    expect(first.getByText("why 1")).toBeInTheDocument();
    expect(first.getByText("Waiting for you")).toBeInTheDocument();
    expect(cards[1]).toHaveAccessibleName("Proposal 2: Cut point");
    await userEvent.click(first.getByRole("button", { name: "Accept Assumption m_1" }));
    await userEvent.click(within(cards[1]).getByRole("button", { name: "Reject Cut point" }));
    expect(onDecide.mock.calls).toEqual([[1, true], [2, false]]);
  });
  it("offers no answer for a decided proposal and says why one failed", () => {
    render(<Proposals ai={ai([p(1, "failed", { error: "property `m_1` already exists" }), p(2, "applied"), p(3, "withdrawn")])} busy={false} onDecide={() => {}} />);
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.getByText("Could not be applied")).toBeInTheDocument();
    expect(screen.getByText("property `m_1` already exists")).toBeInTheDocument();
    expect(screen.getByText("Accepted")).toBeInTheDocument();
    expect(screen.getByText("Withdrawn by the agent")).toBeInTheDocument();
  });
  it("holds the answers while the session is busy", () => {
    render(<Proposals ai={ai([p(1, "pending")])} busy onDecide={() => {}} />);
    for (const b of screen.getAllByRole("button")) expect(b).toBeDisabled();
  });
  it("says so when there is nothing", () => {
    render(<Proposals ai={ai([])} busy={false} onDecide={() => {}} />);
    expect(screen.getByText("No proposals.")).toBeInTheDocument();
  });
});
