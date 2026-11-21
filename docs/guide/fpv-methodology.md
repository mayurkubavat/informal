# Formal Property Verification (FPV) Methodology

This guide covers how to structure clean, sound formal verification environments in InFormal — from design elaboration and reset modeling through vacuity defense, task isolation, and assume-guarantee decomposition.

---

## 1. Standard FPV Setup Flow

Every formal run follows five deterministic stages:

```tcl
# 1. Collect RTL sources, defines, include directories, or VCS filelists
analyze -sv12 +incdir+rtl +define+FORMAL rtl/fifo.sv

# 2. Elaborate with svlang (parameter overrides & optional black boxes)
elaborate -top fifo -parameter DEPTH 8 -create_related_covers {precondition witness}

# 3. Declare clocks and active-high reset condition (held at cycle 0, released after)
clock clk
reset -expression {!rst_n}

# 4. Audit formal setup before writing or proving properties
check_fv_setup -verbose

# 5. Declare environment assumptions, assertions, and reachability covers, then prove
assume -name m_no_ovf {!(full && push)}
assert -name a_count_bound {count <= 4'd8}
cover  -name c_full_drain  {full ##1 pop}
prove -all
```

> **VC Formal equivalent:** `read_file -top fifo -format sverilog -sva -vcs {rtl/fifo.sv}`, `create_clock clk -period 100`, `create_reset rst_n -sense low`, `fvassume` / `fvassert` / `fvcover`, `check_fv`. See the [JG ↔ VCF Crosswalk](../reference/jg-vcf-crosswalk.md).

### Checking Setup Health Early

Run `check_fv_setup` and `get_reset_info` immediately after `clock` and `reset`:

- Flags missing clock/reset declarations, reset-less sequential flops (candidates for `check_xprop`), and undriven internal nets.
- Use `get_design_info` (`report -design`) to verify exact flop, latch, memory, and AIG gate counts before burning solver time.

---

## 2. Writing Sound Properties

### RTL vs. Tcl Properties

Both RTL-embedded assertions (`assert property (...)`, immediate `assert (...)`) and Tcl commands (`assert`, `assume`, `cover`) compile through the exact same `svlang` SVA compiler:

- **RTL properties** live in `<embedded>` with exact RTL file/line source links.
- **Tcl properties** bind in the top module's scope (supporting hierarchical XMRs like `u_sub.state_q` and `top.u_sub.sig`) without modifying RTL files, and validate every identifier immediately so typos never turn into implicit nets.

### The Golden Rule of Assumptions

**Constrain primary inputs only — never constrain DUT outputs or internal state directly.**

Constraining internal DUT state masks real RTL bugs and causes silent over-constraint. When you need an internal invariant to help another proof converge:

1. Write it as an **assertion** (`assert -name a_helper_inv {...}`) and **prove** it first.
2. Promote it safely with **`assume -from_assert a_helper_inv`**. InFormal tracks provenance so `a_helper_inv` is never circularly assumed while proving `a_helper_inv` itself.

---

## 3. Defending Against Vacuity & Over-Constraint

A formal proof under contradictory or dead-end assumptions proves everything vacuously. InFormal guards against this at three layers (detailed in [docs/vacuity.md](../vacuity.md)):

1. **Environment consistency (`check_assumptions -depth 20 -conflict`):**
   Runs automatically before `prove` when `set_vacuity on` (the default). If reset + active assumptions admit no trace of length `N`, `-conflict` extracts a minimal conflicting subset of assumptions.
2. **2QBF dead-end trap-state check (`check_assumptions -dead_end`):**
   Catches subtle *reactive* over-constraints where a reachable DUT state has **no legal primary-input assignment** on the next cycle (the solver could otherwise "hide" a future bug by steering into the trap state).
3. **Precondition & witness covers (`elaborate -create_related_covers {precondition witness}`):**
   - For every proven implication `A |-> B` or `A |=> B`, InFormal checks that antecedent `A` is actually reachable. An unreachable antecedent flags the assertion **`vacuous`**.
   - Enabling `witness` also generates `<prop>:witness` VCD traces showing the property completing non-trivially.
   - Always pair every key requirement with explicit `cover` properties (`c_*`) verifying normal operation, corner states, and full/empty/error transitions remain reachable.

```tcl
check_assumptions -depth 20 -conflict -dead_end
```

---

## 4. Isolating Experiments with Tasks

Every session starts with task `<embedded>`. Use named `task`s to run isolated proof configurations (different assumptions, cut points, or bounded checks) without contaminating signoff properties:

```tcl
# Create a task for an abstract control-path proof
task -create CtrlOnly -copy {a_fsm_*} -copy_assumes -set
stopat u_datapath.alu_res
prove -task CtrlOnly
report -task CtrlOnly

# Return to <embedded> — u_datapath.alu_res remains driven here!
task -set <embedded>
```

Key task rules:
- Properties in task `T` are named `T::prop_name`.
- Assumptions and `stopat` cut points are strictly **per-task**; clocks, reset, and black boxes are design-wide.
- RTL assumptions are copied into every new task and can be disabled per-task (`assume -task T -disable <pat>`).

---

## 5. Deep Proof Auditing (`-with_proof_core`)

Pass `-with_proof_core` to `prove` to extract the minimal subset of assumptions and state support actually used by a proof:

```tcl
prove -property a_count_bound -with_proof_core
get_property_info a_count_bound -proof_core
```

This reveals unnecessary assumptions that can be removed — tightening the verification environment.

---

## Related Guides & Labs

- Hands-on FIFO lab: [Lab 02 — FPV Fundamentals](../labs/02-fpv-fundamentals.md)
- Triaging CEXs: [Debugging Counterexamples](debugging-cex.md)
- Closing `undetermined` proofs: [Convergence & Engines](convergence-and-engines.md)
- Supported SVA syntax: [SVA & Lowering Reference](../reference/sva-support.md)
