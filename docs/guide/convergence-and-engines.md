# Convergence & Engines

When a property returns `undetermined`, blindly raising the timeout rarely works. This guide explains InFormal's engine portfolio and the structured convergence ladder used by formal experts to close hard proofs.

---

## 1. Engine Portfolio Architecture

InFormal runs all applicable engines in parallel per property, takes the **first checked definitive verdict**, cancels the remaining engines, and verifies that no two engines disagree ([docs/engines.md](../engines.md)).

### In-House Engines

| Engine | Level | Strengths |
| --- | --- | --- |
| `informal-bmc` | Bit-level (CaDiCaL) | Fast shallow/medium bug finding (`cex`) and cover witnesses |
| `informal-kind` | Bit-level (CaDiCaL) | Fast proofs on inductive or shallow-diameter designs |
| `informal-ic3` | Bit-level (CaDiCaL) | Unbounded safety proofs via IC3/PDR (single & multi-threaded lemma sharing) |
| `informal-kind-ic3` | Bit-level (CaDiCaL) | Feeds IC3 lemmas into k-induction for deep state machines |
| `informal-klive` | Bit-level (CaDiCaL) | Liveness (`s_eventually`, `s_until`) via k-liveness / L2S |
| `informal-wbmc` | Word-level (Bitwuzla) | Bug finding over wide datapaths and unblasted `Sort::Array` memories (`> 4096` words) |
| `informal-wkind` | Word-level (Bitwuzla) | Word-level k-induction over large arrays and wide arithmetic without bit-blasting |

### External Engines (`config/engines.toml`)

Configured external model checkers (**rIC3**, **ABC**, **Pono**, **BtorMC**) join the same portfolio automatically when present on `PATH`. Restrict engines per session or script with:

```tcl
set_engines informal-bmc informal-kind informal-ic3
# or JasperGold-style letters:
set_engine_mode {Ht B}
```

---

## 2. The Proof Convergence Ladder

When a property is `undetermined`, work down this ladder from lowest-risk to highest-abstraction:

### Step 1 — Profile Complexity (`get_fv_complexity`)

Identify *why* the solver stalled before guessing:

```tcl
get_fv_complexity -property a_hard
get_property_info a_hard -cone
```

Look for:
- **Wide counters / timers** (e.g., 24-bit watchdogs that take millions of cycles to reach terminal counts).
- **Wide multipliers / dividers** (`wide_arith_ops`) sitting inside the COI.
- **Large memories / FIFOs** inflating bit-level state bits.

### Step 2 — Black-Box Irrelevant Heavy Blocks (`blackbox_assistant` & `-bbox_*`)

If heavy datapath blocks (multipliers, SRAMs, crypto cores) sit in the COI without affecting the control property:

```tcl
# Inspect recommendations first, or auto-apply:
blackbox_assistant -threshold_bits 128
blackbox_assistant -threshold_bits 128 -apply

# Or explicitly at elaborate time:
elaborate -top soc_top -bbox_m {sram_1rw} -bbox_mul 16 -bbox_a 256
```

Black-boxing is a **sound over-approximation**: outputs become unconstrained free inputs, so a proof with a black box implies the concrete design holds.

### Step 3 — Counter Jump Abstraction (`abstract -counter`)

Wide timers and watchdogs stall BMC and IC3 because reaching `count == 1_000_000` takes a million transitions. Sound counter abstraction lets the counter fast-forward to `K - 1`:

```tcl
abstract -counter wdog_cnt_q -threshold 1000000
report_abstractions
prove -property c_wdog_expired
```

### Step 4 — Task-Scoped Cut Points (`stopat`) & Helper Invariants (`assume -from_assert`)

Break deep sequential cones cleanly in a dedicated task:

```tcl
# Option A: Cut an internal datapath signal in a separate task
task -create AbstractCtrl -copy {a_hard} -copy_assumes -set
stopat u_alu.res_q
prove -task AbstractCtrl

# Option B: Prove a local inductive invariant first, then promote it safely
assert -name a_ptr_inv {wr_ptr - rd_ptr == count}
prove -property a_ptr_inv
assume -from_assert a_ptr_inv
prove -property a_hard
```

### Step 5 — Bounded Signoff (`-bound K`) & Term Splitting (`informal split`)

- **Bounded proofs:** When unbounded convergence is impractical for a deep datapath, prove a concrete depth guarantee:
  ```tcl
  prove -property a_hard -bound 64   ;# reports `bounded_proven` at depth 64
  ```
- **Wide OR-property splitting:** For standalone AIGER models whose bad state is an OR of many terms, `informal split` proves terms in parallel while sharing certified invariants across terms:
  ```sh
  informal split wide.aig --timeout 300 --jobs 4
  ```

---

## 3. Speeding Up Regressions with Proof Cache

Enable the SHA-256 content-addressed proof cache so unchanged properties return instantly across runs:

```tcl
set_prove_cache on
set_prove_cache_path .informal_cache
set_prove_cache_mode read_write
```

Cache keys hash the exact lowered Btor2 model (design + active assumptions + cut points + property). Any RTL, assumption, or cut-point change automatically misses the cache.

---

## 4. Controlling Memory & Process Isolation

- **`--mem-gb N`**: Caps in-process engine memory (default: 90% of cgroup `memory.max`, else 75% of physical RAM). Exceeding the hard ceiling exits cleanly with code `3`.
- **`--isolate`** (or `INFORMAL_ISOLATE=1`): Runs every in-house engine in its own child process so an OOM or crash in one engine never affects the rest of the portfolio.

---

## Related Guides & Labs

- Hands-on convergence lab: [Lab 03 — Convergence & Abstractions](../labs/03-convergence.md)
- Engine configuration: [Engine & LLM Configuration](../integration/engines-config.md)
- Deep engine notes: [docs/engines.md](../engines.md)
