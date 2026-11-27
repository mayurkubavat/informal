# CEX Triage Playbook

Use this playbook whenever a property settles as `cex` (or when verifying whether a `covered` witness matches legal protocol behavior).

## Step 1: Single-Call Root-Cause Diagnosis

Call `cex_diagnose(name="<prop>")` first. Do NOT call `trace_get` across all signals before inspecting the fused dossier:
- `causal_slice.events`: exact backward causal chain from `failure_frame` to frame 0 (untaken mux arms pruned).
- `signal_roles`: maps every active signal to `primary_input`, `dut_output`, `inout`, or `internal_state`.
- `active_trace`: values of active causal signals over `[failure_frame - window .. failure_frame]`.
- `driver_snippets`: RTL assignment lines driving the active DUT state/outputs.
- `lint_findings`: `SVA-FPV-*` issues (e.g., `SVA-FPV-003` missing `disable iff`).

## Step 2: Classify Into Exactly One Root-Cause Bucket

### Bucket A — Genuine RTL Bug
**Indicators:**
- All active `primary_input` transitions are legal under the interface protocol, yet a `dut_output` or `internal_state` driver snippet computes the wrong next-state/output.
- Or `signal_roles` shows no unconstrained primary inputs in the causal chain.

**Action:**
1. Cite exact failure cycle, preceding causal transitions, and RTL `file:line`.
2. Do **NOT** propose an `assume` to hide the failure.
3. If a desktop window is connected, call `gui_present_cex(name="<prop>")` to show the engineer the exact failure path in `cex-debug`.

### Bucket B — Missing Legal Environment Assumption
**Indicators:**
- A `primary_input` takes an illegal protocol combination (e.g., `push && full` on a no-overflow interface, deasserting `valid` before `ready`, illegal opcode/burst change mid-transfer).
- If the spec is ambiguous about whether that input combination is illegal or must be tolerated by RTL, **ask the engineer** rather than guessing.

**Action:**
1. Formulate the **weakest** primary-input-only constraint describing the legal environment.
2. Call `proposal_validate(kind="assume", expr="...", targets=["<prop>"])` BEFORE `proposal_add`:
   - Confirm `environment.conflict == false` and `dead_end.found == false`.
   - Confirm `lost_covers` is empty (zero previously reachable covers killed).
   - Confirm `vacuous_assertions` is empty and `overconstraint_warnings` is empty.
   - Confirm `<prop>` appears in `newly_proven`.
3. Only if `proposal_validate` passes cleanly, call `proposal_add` with a concise rationale summarizing the validation scorecard (and remember proofs will be tagged `under_ai`).

### Bucket C — Property or Setup Bug
**Indicators:**
- `SVA-FPV-003` fired: assertion missing `disable iff (<reset>)` and fails at cycle 0/1 during reset.
- Off-by-one sampling (`|->` vs `|=>`) or `$past(x)` evaluated on cycle 0 before a prior cycle exists.
- Wrong clock edge or reset polarity in setup script.

**Action:**
- For `SVA-FPV-003` in RTL, offer `formal_fix_apply` (respecting mode).
- For script/SVA expression bugs, validate the corrected property with `proposal_validate(kind="assert", expr="...")` and explain the exact temporal mismatch.

## Step 3: Visual Communication Etiquette
- With `gui_*` tools available, call **`gui_present_cex`** once (instead of chaining 5 separate `gui_*` calls). Let `gui_present_cex` auto-populate steps from `why_cex`, or pass concise 2–5 step `steps` + `annotations` tailored to your diagnosis.
- Never silently move the engineer's window while they are manually exploring unless explaining a concrete finding.
