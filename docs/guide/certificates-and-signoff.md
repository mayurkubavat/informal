# Certificates & Formal Signoff

High-assurance formal verification requires two complementary guarantees:
1. **Engine trust (mathematical soundness):** Did the solver actually prove the property?
2. **Plan completeness (verification signoff):** Did we prove the *right* requirements without vacuity, over-constraints, dead-ends, or unreviewed AI changes?

InFormal addresses both layers natively.

---

## 1. Independent Proof & CEX Certificates (`--certificate`)

Before InFormal reports any verdict internally, it checks every IC3/k-induction invariant against the model and replays every counterexample. For **zero-shared-code trust**, pass `--certificate <DIR>` to export machine-checkable artifacts verified by independent third-party checkers ([docs/certificates.md](../certificates.md)):

### Standalone Models (`informal check`)

```sh
informal check tests/models/delay_safe.aag --certificate certs/
python3 -I scripts/certificate_check.py certs/
```

### FPV Shell Sessions (`informal shell`)

```sh
informal shell examples/05-integration-and-signoff/signoff-spec/run.jg.tcl --certificate certs/
python3 -I scripts/certificate_check.py certs/
```

| Verdict | Artifact written | Independent checker |
| --- | --- | --- |
| `SAFE` / `proven` | AIGER witness circuit (`.wit.aig`) + `.cert.json` | **Certifaiger** |
| `UNSAFE` / `cex` | AIGER witness trace (`.wit`) + `.cert.json` | **`aigsim`** / **`btorsim`** |

---

## 2. Machine-Verifiable Formal Verification Plans (`fvp.spec.json`)

For design signoff, pair your Tcl script with a structured **`fvp.spec.json`** file (schema `informal.spec/1`; complete schema in [Formal Spec Schema Reference](../reference/spec-schema.md)).

A verification plan encodes four enforceable contracts:

1. **Design intent (`design`):** Expected `top` module, `clocks`, and `reset` expression — prevents silently proving the wrong top or inverted reset.
2. **Assumption boundary (`boundary`):** Explicitly lists `allowed_assume_inputs` and `forbidden_assume_signals` (such as DUT outputs or internal state) so over-constraints are caught mechanically.
3. **Requirement traceability (`requirements[]`):** Maps each `REQ-*` ID to its target `assertions`, anti-vacuity witness `covers`, and associated `assumptions`.
4. **Signoff gates (`signoff`):** Sets hard pass/fail thresholds:
   - `proof_target`: `"proven"` (unbounded) or `"bounded_proven"` (with `min_bound`)
   - `max_overconstraints: 0`
   - `max_dead_ends: 0` (optionally with `require_dead_end_check: true`)
   - `forbid_vacuous: true`
   - `require_covers: true`
   - `forbid_under_ai: true` (blocks signoff until AI-proposed assumptions/cut points are reviewed and promoted into permanent RTL/Tcl)
   - Optional `coi_coverage_floor` and `mutation_score_floor`

---

## 3. Auditing Signoff with `spec_audit`

In an MCP session (or via the `audit_signoff` MCP Prompt), call `spec_audit`:

```json
{
  "spec_path": "examples/05-integration-and-signoff/signoff-spec/fvp.spec.json",
  "check_dead_end": true
}
```

`spec_audit` evaluates the live session against every gate and returns:
- `signoff_ready`: `true` or `false`
- Per-requirement verdicts: `verified`, `cex`, `undetermined`, `missing_assert`, `missing_cover`, `vacuous`, `overconstrained`
- Unmapped assertions and boundary violations
- Concrete `next_actions` hints for any failing gate

---

## Related Guides & Labs

- Complete runnable signoff + certificate + Makefile lab: [Lab 05 — CI, Certificates & Signoff](../labs/05-integration-and-signoff.md)
- Certificate internals: [docs/certificates.md](../certificates.md)
- Full `informal.spec/1` field reference: [Formal Spec Schema](../reference/spec-schema.md)
