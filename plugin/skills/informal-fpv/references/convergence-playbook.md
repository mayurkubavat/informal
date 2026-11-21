# Proof Convergence Playbook (`undetermined` & Slow Proofs)

Use this playbook when `prove` returns `undetermined` (timeout / memout / bound reached) or takes excessively long.

## Step 1: Single-Call Convergence Dossier

Call `convergence_diagnose(name="<prop>")` first. Inspect:
- `property.stats`: `max_bound` reached by BMC, `depth_kind` (`k` / `frames` / `bound`), `invariant_clauses`, `cpu_ms`, `peak_rss_kb`.
- `cone`: `inputs` and `registers` count + nearest sequential registers by distance.
- `complexity`: wide arithmetic hotspots (`mul`, `div`, wide adders), memory arrays, and blackbox candidates.
- `recommendations`: ranked starting tactics.

## Step 2: Match Tactic to Bottleneck Signature

### Tactic 1 — Inductive Weakness (Deep BMC Bound, Zero CEX, IC3 Stalls)
**Signature:** `max_bound >= 15` with no counterexample, modest register count, no huge multipliers/memories. The assertion holds from reset, but k-induction / IC3 cannot exclude unreachable bad predecessor states on its own.

**CEGIS Helper-Lemma Loop:**
1. Inspect the RTL registers in `cone.sample_registers` (`design_source` / `signal_nav`) for structural invariants the property depends on:
   - FIFO / queue: `count == (wp - rp)` (or wrap-bit invariant), `empty == (count == 0)`, `full == (count == DEPTH)`.
   - FSM: one-hot encoding (`$onehot(state)`), illegal state exclusion, valid-counter correlation.
   - Pipeline / credit counter: `credits + inflight == MAX_CREDITS`.
2. Test each candidate helper invariant with **zero side effects**:
   ```json
   proposal_validate(kind="assert", name="h_inv_1", expr="...", targets=["<prop>"])
   ```
   `proposal_validate` proves the candidate lemma itself AND (if proven non-vacuous) promotes it via `assume -from_assert` in the scratch task to test whether `<prop>` now closes!
3. If `candidate_result.status == "cex"`, inspect why the lemma is false and weaken/fix it.
4. Once `verdict == "pass"` and `<prop>` is in `newly_proven`, propose the helper assertion with `proposal_add` (it is a **sound** proven invariant, not an unverified environment assumption!).

### Tactic 2 — Wide Datapath / Arithmetic Hotspot (`mul`, `div`, CRC, wide muxes)
**Signature:** `complexity` flags wide arithmetic operators or datapath width dominates COI.

**Action:**
1. Check whether the target assertion actually depends on the exact arithmetic value or only on control/handshake/tag flow.
2. If control-only, test an over-approximate abstraction:
   ```json
   proposal_validate(kind="cut_point", signal="<datapath_out>", targets=["<prop>"])
   ```
   Remember: a `cut_point` (`stopat`) **over-approximates** the RTL (it frees the signal). A `proven` verdict under a `cut_point` is **100% sound** for the real design; only a `cex` may be spurious.
3. If a `cut_point` causes a spurious CEX, either move the cutpoint earlier or replace it with a proven helper assertion constraining the cutpoint's envelope.

### Tactic 3 — Deep Memories / Large Parameterized Structures
**Signature:** `complexity` flags memory arrays or high state-bit count from `DEPTH`/`WIDTH`.

**Action:**
- Verify control/ordering properties with reduced parameters (`elaborate -param DEPTH_LOG2=2 -param DW=4`) or symbolic-entry/cutpoint abstractions, and explicitly state parameter bounds in the report.

### Tactic 4 — Mode / Case Explosion
**Signature:** High `inputs` count or multiple mutually exclusive modes (e.g., read vs write, burst types, channels) slow down IC3.

**Action:**
- Partition into mutually exclusive tasks (`task -create mode_a`, `task -create mode_b`) with a companion `assert` verifying the case split is exhaustive (`mode_a || mode_b`).

## Step 3: Mandatory Soundness Check
Before reporting convergence:
- Verify `vacuity == "non-vacuous"` on every closed assertion.
- Report every `under_ai` assumption or `cut_point` explicitly; never hide abstractions.
