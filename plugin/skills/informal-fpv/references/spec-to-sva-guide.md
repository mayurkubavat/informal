# Spec-to-SVA Engineering Guide

Use this guide when translating natural-language specifications, bus protocols, or CSR specs into verifiable formal properties in InFormal.

## Step 1: Establish the Boundary Contract First

Before writing any SVA:
1. Call `design_hierarchy` to inspect the top module's exact port list, widths, and directions (`input` vs `output` vs `inout`).
2. Call `session_summary` (and `design_source` if needed) to confirm exact clock edge, reset signal, and active reset polarity (`!rst_n` vs `rst`).
3. Classify every spec clause strictly:
   - **Obligations on DUT outputs / internal state** → `assert` (never `assume`).
   - **Preconditions on primary inputs / peer protocol** → `assume` (weakest legal rule only).
   - **Reachability scenarios & antecedent sanity checks** → `cover`.

## Step 2: Prefer Built-In Formal Apps Over Hand-Rolling Boilerplate

Before manually writing dozens of properties:
- **Standard bus protocol** (`AXI4-Lite`, `TL-UL`, `APB`): use `check_protocol` (`Protocol` task).
- **CSR / Register bank** (OpenTitan HJSON, CSV, or `uvm_reg_block`): use `check_csr` (`CSR` task).
- **Pin-to-pin / mux connectivity table**: use `check_conn` (`Connectivity` task).
- **Equivalence against golden RTL / reference**: use `check_sec` (`SEC` / `LEC`).
- **X-propagation or security information flow**: use `check_xprop` / `check_spv`.

## Step 3: SVA Authoring Checklist (Avoid Common LLM Pitfalls)

1. **Always guard against reset**: Include `disable iff (<reset_expr>)` in RTL SVA (or verify InFormal's session `reset` covers command-added properties) so `SV-FPV-003` never trips.
2. **Exact temporal operator**:
   - Same-cycle combinational or registered consequence: `A |-> B`.
   - Next-cycle consequence: `A |=> B` (equivalent to `A |-> ##1 B`).
3. **Protect `$past(...)` at cycle 0**: Use `|=>` or gate with a post-reset cycle condition so `$past` is never sampled before a valid prior cycle.
4. **Always pair every non-trivial implication with a witness `cover`**:
   - For `assert -name a_foo {A |=> B}`, also validate `cover -name c_foo_wit {A ##1 B}` (or `cover {A}`).
   - If the cover is `unreachable`, the assertion is vacuously true!
5. **Avoid hidden over-constraints in assumptions**:
   - Never write `assume {dut_out |-> in_sig}` if it can be satisfied by constraining `dut_out` to 0 (classic formal cheat).
   - Use `proposal_validate(kind="assume", ...)` to verify `environment.conflict == false`, `dead_end.found == false`, `overconstraint_warnings == []`, and `lost_covers == []`.

## Step 4: Verify Before Proposing

For every synthesized property:
1. Call `proposal_validate(kind="assert"|"cover"|"assume", expr="...", name="...")`.
2. For an `assert` that comes back `cex`, inspect with `cex_diagnose` to determine whether the RTL has a real bug or the SVA mis-modeled the spec.
3. For a `cover`, confirm `candidate_result.status == "covered"`.
4. Only call `proposal_add` once validated, including the spec requirement ID and validation result in `rationale`.

## Step 5: Machine-Verifiable Plan & Signoff (`fvp.spec.json` + `spec_audit`)

Author or audit a compact `fvp.spec.json` (`"schema": "informal.spec/1"`) alongside the RTL so requirement completeness and boundary soundness are verified mechanically:

```json
{
  "schema": "informal.spec/1",
  "design": { "top": "fifo", "clocks": ["clk"], "reset": "!rst_n" },
  "boundary": {
    "allowed_assume_inputs": ["push", "pop", "din"],
    "forbidden_assume_signals": ["full", "empty", "dout", "status", "count", "wp", "rp"]
  },
  "requirements": [
    {
      "id": "REQ-FIFO-001",
      "title": "Occupancy stays within depth and full/empty are mutually exclusive",
      "assertions": ["a_count_max", "a_full_empty"],
      "covers": ["c_full"]
    }
  ],
  "signoff": {
    "proof_target": "proven",
    "max_overconstraints": 0,
    "max_dead_ends": 0,
    "forbid_vacuous": true,
    "coi_coverage_floor": 90.0
  }
}
```

- Run `spec_audit(check_dead_end=true)` (auto-discovers `fvp.spec.json` next to the open design files, or accepts `spec_file` / inline `spec`).
- Require `signoff_ready == true` before claiming verification signoff: zero `missing_assert`, zero `missing_cover`, zero `vacuous`, zero `overconstrained`, and all `gates` green.
