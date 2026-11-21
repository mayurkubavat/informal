# Lab 02 — FPV Fundamentals (Synchronous FIFO)

**Level:** Intermediate (15 minutes) · **Directory:** `examples/02-fpv-fundamentals/fifo/`  
**Concepts:** Temporal SVA (`##1`, `|->`, `|=>`, `$past`), submodule XMR (`u_wp.p`), `check_fv_setup`, `get_design_info`, precondition & witness covers, `check_assumptions -conflict -dead_end`, `-with_proof_core`, isolated `task`, and `why_cex`.

## Run It

```sh
informal shell examples/02-fpv-fundamentals/fifo/run.jg.tcl
```

## What This Lab Demonstrates

1. **Setup & design audit (`check_fv_setup`, `get_design_info`):** Checks clocks, reset, flop/memory counts, and AIG size before proving.
2. **Precondition & witness covers (`-create_related_covers {precondition witness}`):** Verifies every proven implication (`a_no_overflow`, `a_status`, `a_data`) has a reachable antecedent (non-vacuous) and records witness VCD traces.
3. **2QBF dead-end check (`check_assumptions -depth 12 -conflict -dead_end`):** Confirms the assumption environment is `alive` with no reactive trap states.
4. **Task isolation (`task -create CexDemo`):** Adds an intentional failing assertion (`CexDemo::a_wrong`) and slices its CEX with `why_cex` without touching `<embedded>`.

## Expected Verdicts

- `<embedded>` assertions (`a_count_max`, `a_full_empty`, `a_ptr_diff`, `a_no_overflow`, `a_status`, `a_data`, `a_sub_ptr`): all **`proven`** (non-vacuous)
- `<embedded>` covers: `c_full` & `c_status3` **`covered`**, `c_count5` **`unreachable`**
- `CexDemo::a_wrong`: **`cex`** (depth 4)

## Try It Yourself

1. **Trigger a dead-end trap:** Add an illegal DUT-output assumption `assume -name m_bad {!full}` and run `check_assumptions -dead_end`.
2. **Inspect proof support:** Run `get_property_info a_data -cone` and `get_property_info a_data -proof_core`.
