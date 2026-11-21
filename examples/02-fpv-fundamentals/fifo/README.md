# Lab 02 — FPV Fundamentals (Synchronous FIFO)

**Level:** Intermediate (15 minutes)  
**Concepts:** Temporal SVA (`##1`, `|->`, `|=>`, `$past`), submodule XMR (`u_wp.p`), `check_fv_setup`, `get_design_info`, precondition & witness covers, `check_assumptions -conflict -dead_end`, `-with_proof_core`, isolated `task`, and `why_cex`.

## Run It

```sh
informal shell examples/02-fpv-fundamentals/fifo/run.jg.tcl
```

## What This Lab Demonstrates

1. **Setup & design audit (`check_fv_setup`, `get_design_info`)** — verifies clocks, reset, flop/memory counts, and AIG size before proving.
2. **Precondition & witness covers (`-create_related_covers {precondition witness}`)** — ensures every proven implication (`a_no_overflow`, `a_status`, `a_data`) has a reachable antecedent (not vacuous) and records witness VCD traces.
3. **2QBF dead-end check (`check_assumptions -dead_end`)** — confirms the environment has no trap states.
4. **Task isolation (`task -create CexDemo`)** — adds an intentional failing assertion (`CexDemo::a_wrong`) and slices its CEX via `why_cex` without polluting `<embedded>`.

## Expected Verdicts

- `<embedded>` assertions (`a_count_max`, `a_full_empty`, `a_ptr_diff`, `a_no_overflow`, `a_status`, `a_data`, `a_sub_ptr`): all **`proven`** (non-vacuous)
- `<embedded>` covers: `c_full` & `c_status3` **`covered`**, `c_count5` **`unreachable`**
- `CexDemo::a_wrong`: **`cex`** (depth 4)

## Try It Yourself

1. **Trigger a dead-end trap:** Add a bad assumption like `assume -name m_bad {!full}` (constraining a DUT output!). Run `check_assumptions -dead_end` and watch InFormal catch the over-constraint.
2. **Inspect the COI:** Run `get_property_info a_data -cone` and `get_property_info a_data -proof_core`.
