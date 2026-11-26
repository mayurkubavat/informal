# Debugging Counterexamples (CEX)

When an assertion fails (`cex`) or a cover hits (`covered`), InFormal automatically replays the witness against the original design and writes a VCD trace under `--workdir`. This guide shows how to move from a raw failure to root cause in minutes.

---

## 1. The 4-Step CEX Triage Loop

Every `cex` falls into exactly one of three root-cause buckets:

1. **RTL bug** — legal primary inputs trigger an illegal DUT state or output.
2. **Missing environment assumption** — an unconstrained primary input took an illegal protocol value.
3. **Broken SVA / setup error** — wrong reset polarity, off-by-one `|->` vs `|=>`, or clock mismatch.

Use this progression to classify cleanly:

```tcl
# 1. Inspect verdict, depth, and source location
get_property_info a_never9

# 2. Extract the dynamic backward causal root-cause slice
why_cex -property a_never9

# 3. Inspect the Cone of Influence (COI) inputs vs. state registers
get_property_info a_never9 -cone

# 4. Open the VCD waveform in your viewer (or InFormal.app)
visualize -violation -property a_never9
```

---

## 2. Dynamic Backward Causal Slicing (`why_cex`)

Counterexample traces on real IP blocks can contain thousands of signals. `why_cex` walks backward from the failure cycle `T` to cycle `0` over the actual trace values, pruning untaken mux arms and non-controlling Boolean operands so only the **active causal chain** remains:

```tcl
why_cex -property a_never9 -max_events 25
why_cex -property a_never9 -json    ;# structured JSON for scripts or AI tools
```

What to look for in the causal slice:
- **Primary inputs at the root of the slice:** Check whether the input values shown violate the upstream interface protocol. If yes, constrain *only that primary input* with an `assume` (never constrain internal DUT signals).
- **RTL register/combinational assignments inside the slice:** If inputs are completely legal, trace the active mux/branch condition straight to the RTL bug.

---

## 3. Waveform & Source Inspection

### From the Tcl Shell

Set `INFORMAL_WAVE_VIEWER` (for example `surfer` or `gtkwave`) before running `informal shell`:

```sh
export INFORMAL_WAVE_VIEWER=surfer
informal shell examples/01-quickstart/counter/run.jg.tcl
```

Inside the shell:

```tcl
visualize -violation -property a_never9   ;# opens CEX VCD (and returns its path)
visualize -cover -property c_done         ;# opens witness cover VCD
```

### In the Desktop Workbench (`InFormal.app`)

In the **`CEX Debug`** workspace preset:
- Click any `cex` / `covered` property to synchronize **SourceView** (with inline `@C<k>` signal values at the cursor cycle), **SchematicView** (COI subgraph highlighting active `why_cex` edges), and **TraceView** (waveform with `Why CEX` signal filtering and causal callout badges).
- Click any signal or cycle in `Why CEX` to jump the waveform cursor and highlight the driving RTL line.

---

## 4. Formal Lint (`SVA-FPV`) & Quick-Fixes

Before chasing an elusive CEX, run `svlang`'s formal-readiness lint pack (`formal_lint` via MCP or the desktop workbench's Lint diagnostics) to catch common formal setup bugs automatically:

- **`SVA-FPV-001`**: Assumption constraining a DUT output or internal signal instead of primary inputs.
- **`SVA-FPV-002`–`005`**: Missing reset guards, uninitialized flops, latch inference, and SVA/CDC/FSM traps.

Many findings include 1-click byte-range quick-fixes (`Quick-Fix` in `SourceView`, or `formal_fix_apply` via MCP).

---

## 5. AI-Assisted Single-Call Triage (`cex_diagnose` & `gui_present_cex`)

When working with an AI agent connected to `informal mcp`:
- **`cex_diagnose`** fuses `property_get`, `why_cex`, a trimmed causal trace, primary-input vs. DUT-state roles, RTL driver source snippets, and `formal_lint` into one JSON dossier.
- **`proposal_validate`** tests any proposed assumption or assertion fix in a zero-side-effect scratch task (checking syntax, `SVA-FPV-001`, conflicts, 2QBF dead-ends, and cover non-regression) *before* proposing it.
- **`gui_present_cex`** switches a live `InFormal.app` window to `CEX Debug`, pins causal callouts, and launches a step-by-step walkthrough bar.

---

## 6. Debugging Solver Disagreements (`informal reduce`)

If two engines ever return contradictory definitive verdicts on a bit-level AIGER model (flagged automatically by the portfolio runner), delta-debug to a minimal reproducer:

```sh
informal reduce failing.aig --disagree informal-ic3,ric3-ic3 -o minimal.aag
```

---

## Related Guides

- [FPV Methodology](fpv-methodology.md)
- [Desktop Workbench UI](workbench-ui.md)
- [AI & MCP Integration](../integration/ai-and-mcp.md)
- [Trace Format Internals](../ui/trace-format.md)
