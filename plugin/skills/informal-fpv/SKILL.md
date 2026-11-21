---
name: informal-fpv
description: Expert IEEE 1800 SystemVerilog formal property verification with InFormal: single-call CEX root-cause diagnosis (`cex_diagnose`), proof-convergence triage (`convergence_diagnose`), zero-side-effect CEGIS soundness validation (`proposal_validate`), RTL cone schematics, Formal Apps (COV, X-Prop, SPV, SEC, CSR, Protocol VIP), and interactive workbench walkthroughs.
---

# InFormal Formal Verification — Expert Playbook

Use the `informal` MCP server tools whenever the user wants to elaborate SystemVerilog RTL, synthesize or prove SVA properties, debug counterexamples (CEX), close `undetermined` proofs, run Formal Apps, or drive the interactive workbench.

## Non-Negotiable Soundness Invariants (Decision D17)

1. **Engine verdicts only**: Never claim a property holds unless `prove` returns `proven` (and `vacuity` is `non-vacuous`). Proposals are not verdicts.
2. **Primary-input boundary rule**: Environment `assume`s may constrain **primary inputs / interface protocol boundary** only. Never put DUT outputs or internal registers in an `assume` to make a failing assertion pass — if an internal invariant is needed for induction, prove it as an `assert` first and promote it (`assume -from_assert`).
3. **Validate before proposing**: Always run `proposal_validate` (zero-side-effect scratch-task oracle) BEFORE `proposal_add`. Never propose anything with `verdict: "reject"` (environment conflict, 2QBF dead-end trap state, lost cover, vacuous proof, or failing lemma).
4. **Never mask an RTL bug**: When legal primary-input stimulus violates a spec assertion, report the RTL bug with exact cycle, signal values, and driver line — do NOT invent an assumption to hide it.
5. **Pair assertions with witness covers**: Every non-trivial implication (`A |-> B` / `A |=> B`) should have a companion `cover` confirming its antecedent is reachable.

## High-Signal Tool Routing (Prefer Composite Tools First)

| Situation | First Tool to Call | Follow-Up / Playbook |
| --- | --- | --- |
| Open design / script | `session_open` → `formal_lint` | Fix `SV-FPV-001..005` early (`formal_fix_apply`) before trusting proofs |
| Run proofs | `prove` | Check `vacuity` and `under_ai` on every settled property |
| Property is `cex` or `covered` | `cex_diagnose` | Read `references/cex-triage-playbook.md` (do NOT dump raw `trace_get` first) |
| Property is `undetermined` or slow | `convergence_diagnose` | Read `references/convergence-playbook.md` (helper-lemma CEGIS / abstraction) |
| Testing a candidate `assume` / lemma / `cut_point` | `proposal_validate` | Only call `proposal_add` when `ready_to_propose: true` and warnings are justified |
| Formalizing a spec requirement into SVA | `design_hierarchy` + `design_source` | Read `references/spec-to-sva-guide.md` |
| Desktop `InFormal.app` connected (`gui_*` present) | `gui_present_cex` | Single-call atomic visual tour (`cex-debug` + Why CEX + callouts + walkthrough) |
| IDE embedded side pane (no `gui_*`) | `open_workbench` | Automatically refreshed by `prove`, `cex_diagnose`, `why_cex`, `schematic_get`, Formal Apps |
| Formal Apps (COV, X-Prop, SPV, SEC, CSR, VIP) | `check_cov` / `check_xprop` / `check_spv` / `check_sec` / `check_csr` / `check_protocol` | Inspect `session_summary` and `property_list` after running |

## Server-Hosted MCP Prompts

When starting a structured workflow, you can also inspect or invoke the session-hydrated MCP prompts (`prompts/list` / `prompts/get`):
- `triage_cex`: live-hydrated CEX root-cause classification & validation protocol.
- `close_undetermined`: live-hydrated proof convergence & helper-lemma CEGIS ladder.
- `spec_to_sva`: hydrated with exact top-module `inputs` vs `outputs` boundary to prevent output-constraining assumptions.
- `audit_signoff`: full formal signoff gate (`formal_lint`, vacuity, `under_ai`, cover sanity, `check_cov`).

## Progressive-Disclosure Reference Guides

Load these on demand only when working on the corresponding task:
- `references/cex-triage-playbook.md`: Decision tree for RTL bug vs missing assumption vs broken SVA, plus visual walkthrough etiquette.
- `references/convergence-playbook.md`: Diagnostic ladder for `undetermined` proofs — helper lemma mining (CEGIS), `cut_point` abstraction, blackboxing, and case splitting.
- `references/spec-to-sva-guide.md`: Spec decomposition into verifiable SVA + witness cover pairs, reset/clock pitfalls, and Formal App selection.
