# Comparison with SymbiYosys and EBMC

InFormal's claim is a whole flow: SystemVerilog with properties in, a
verdict per property out. This directory measures that claim against the
two open flows that make the same one, on designs anybody can fetch:

- which designs and which properties each flow reads,
- whether the flows give the same verdicts,
- how long each takes (only in a run made on a quiet machine).

The result is `docs/compare.md`, written by `compare.py report` from a
run file in `results/`. No commercial tool appears anywhere here.

## Running it

```sh
bench/compare/compare.py tools     # install or check the two comparison tools
bench/compare/compare.py fetch     # fetch the third-party designs
cargo build --release -p informal
bench/compare/compare.py run       # every case through every flow
bench/compare/compare.py report    # docs/compare.md from the newest run file
```

`run --only <regex>` takes some cases, `--flows` some flows. A run appends
one JSON line per tool run to `results/<id>.jsonl`: the case, the flow and
its configuration, exit status, wall and CPU seconds, the machine load,
whether the design was read, and every property with the tool's own
status word and the verdict it was mapped to. The tools' full logs and
work directories stay in `$INFORMAL_COMPARE_DIR/runs/<id>` (default
`~/.cache/informal/compare`), next to the fetched designs.

A verdict run can be shared between machines: each runs some cases
(`--only`) into a file of its own (`--out`), with `COMPARE_MACHINE` set to
a short name that is recorded with every result, and the files are
concatenated. A later record for the same case, flow and configuration
replaces an earlier one. The tools must be the pinned ones on every
machine (`compare.py tools` checks), and InFormal the same commit.

`run --purpose timing` is the only kind of run whose times go into the
report, and it refuses to start when the machine is busy. The default,
`--purpose verdicts`, records times too but the report leaves them out.

## The tools

Pinned in `tools.toml` by release file and SHA-256, installed beside the
tools of `tools.lock` and never part of it: they are outside programs that
this script starts, not linked, not vendored, not used by InFormal
(decision D14 stands).

| Flow | What runs | Version |
| --- | --- | --- |
| InFormal | `informal shell` on a generated script: `analyze`, `elaborate`, `clock`, `reset`, `prove -all` | the commit of this checkout |
| SymbiYosys | `sby` from the OSS CAD Suite, with Yosys, the yosys-slang front end and the solvers of the same release | OSS CAD Suite 2026-11-15: SBY v0.70-9, Yosys 0.71+34 |
| EBMC | `ebmc` from the release package | 6.2 |

SymbiYosys also has a commercial front end (Verific, in Tabby CAD) that
reads much more of SVA. It is not used: the comparison is between open
flows, and that front end's licence does not allow publishing results.

## How each flow is run

Every flow gets the same source files, the same top module, the same
defines and include directories, and the same clock and reset.

**Reset.** InFormal's `reset -expression` holds the reset in cycle 0 and
releases it after. EBMC's `--reset` does the same. SymbiYosys has no reset
command, so its runs get one small module bound to the top that assumes
the same thing (`RESET_BIND` in `compare.py`). A design without a reset
in `cases.toml` gets none in any flow.

**InFormal.** Two configurations: `default` is the portfolio of
`config/engines.toml`, which includes the external checkers rIC3 and ABC
when they are installed; `inhouse` is InFormal's own engines only. A
property's verdict is whatever the tool reports. The per-property time
limit is `--prop-timeout` (60 s).

**SymbiYosys.** A design that ships with its own `.sby` file (the
SymbiYosys examples, common_cells) is run with that file, unchanged, one
run per task named in `cases.toml`. Every other design gets a generated
`.sby` that reads the sources with `read_slang --formal` and runs three
tasks: `smtbmc` (mode `prove`: bounded check and k-induction with the
default solver), `pdr` (mode `prove`, `abc pdr`) and `cover` (mode
`cover`). `smtbmc` is given `--keep-going`, which is not the default: without
it the first failing assertion ends the run and every other assertion is
left without a status.

**EBMC.** EBMC has no default engine beyond a bounded check of depth 1,
so four configurations are run: `bmc` (`--bound N`), `kind`
(`--k-induction --bound N`), `bdd` and `ic3`.

**The reference count.** Each design is also elaborated by the stand-alone
slang of the suite, and the assert and cover statements in its syntax tree
are counted. That number is what a flow should have found; it does not
depend on any of the three flows.

The bound N is 20 for both tools' bounded engines, SymbiYosys's default
depth (`--depth`). A shipped `.sby` keeps its own depth.

**One verdict per flow.** A flow's verdict on a property is the first
definite one over its configurations: proven, counterexample, covered or
unreachable. "No counterexample up to depth N" is reported as *bounded*,
not as proven. If two configurations of one flow contradict each other,
the report says so.

## The designs

`cases.toml` lists them, in four sets, and says for each whose ground it is.

| Set | Cases | From |
| --- | ---: | --- |
| InFormal's own tests | 15 | `tests/sv` designs that have properties in the RTL and a script of one clock and a reset and no assumption given by command; `tests/fpv` counter and fifo with the properties of their scripts written in a bound module (`props/`) |
| SymbiYosys examples | 14 | the examples that ship in the OSS CAD Suite, with their `.sby` files |
| EBMC examples | 123 | `examples/Benchmarks` of the EBMC repository at tag `ebmc-6.2` |
| Third-party designs | 13 | pulp-platform common_cells with the properties of its `formal/` directory; OpenTitan primitives with the formal test benches of `hw/ip/prim/fpv` |

Left out, with the reason:

- `tests/sv` designs whose script needs more than a clock and a reset (cut
  points, black boxes, several related clocks, lowering switches), whose
  script is expected to fail, or that have command properties only. The
  command properties of the included designs are left out too: they exist
  in InFormal's script form only.
- SymbiYosys examples `indinv` and `abstract` (their scripts need the
  commercial front end), `demos/memory` and `demos/up_down_counter` (the
  source is inside the `.sby` file), and the examples of other tools in
  the suite (equivalence, mutation, VHDL).
- common_cells `cdc_reset_ctrlr` (its file list is generated by a build tool).
- OpenTitan `prim_lfsr` and `prim_packer` (the test benches drive one
  variable from several instances, which the standard does not allow),
  and the `prim_secded` family (the property files rely on a macro file
  compiled before them in the same unit).

## Rules

- A run records every refusal, crash and time-out; nothing is dropped
  from the run file, and the report lists every design a flow did not read.
- A property that gets two different definite verdicts is listed in the
  report and explained under "Differences" below before any number from
  that run is quoted.
- InFormal is not tuned to this set. A change made to InFormal or svlang
  because of a result here is listed under "Changes made because of this
  comparison", and the designs it affects are named.
- A new tool version means a new run, not an edited table.

## Differences

Each property that got two different definite verdicts in a run, and what
was found when it was looked at. "Looked at" means the tool's own trace and
log were read; none of this has been confirmed with the other tools'
authors.

- **`informal-tests/both_edges`, `a_neg`: InFormal proven, EBMC
  counterexample.** The design has a register on each clock edge and the
  property is clocked on the falling edge. EBMC's model has no clock: its
  trace shows `clk = ?` and one step for both edges, with the rising-edge
  register still unreset in the state the reset holds. InFormal models
  both edges (the reset covers the whole first cycle) and gives the answer
  the test records. Read as a modelling difference on a two-edge design,
  not as a wrong proof.
- **`informal-fpv/counter`, `a_never9`: EBMC's bounded check finds a
  counterexample, EBMC's BDD engine says proved.** InFormal also finds the
  counterexample (depth 10) and it is the answer the test records. The BDD
  run prints "SAT checker inconsistent" on other designs when `--reset` is
  given, so its proofs under a reset are not counted as agreement or
  disagreement with anything; the flow's verdict is the counterexample.
- **`sby-examples/fifo-nofullskip`, covers `w_underfill` and
  `w_overfill`: InFormal unreachable, EBMC covered.** With the skip logic
  defined away, an address can only move when its enable is set, which
  the cover excludes. EBMC's cover trace for `w_underfill` has two states
  and the covered expression is false in both (`past_nwen` is 0). A
  SymbiYosys cover run on the same define (depth 40, not part of the
  recorded run) does not reach either cover. Read as a fault in EBMC's
  handling of an immediate cover inside a clocked block under a condition.
- **Covers under EBMC's IC3 engine.** It reports REFUTED for covers that
  EBMC's own bounded check reaches with a trace (`sby-examples/fifo`
  `w_nreset`, `quickstart-cover`): it answers the cover as an assertion of
  the same expression. `compare.py` therefore takes no cover verdict from
  that engine.

Not differences between tools, but faults of the first set-up that were
corrected before the recorded run: a `tests/sv` design whose script adds
an assumption by command was compared without it (such designs are now
left out), and a bounded SymbiYosys pass was counted as a proof.

## What the comparison found in InFormal

Recorded as found, at the commit of the run; a fix is a change made
because of this comparison and is listed in the next section when it lands.

(At `6dde4a5`; the first three are what `98b4b81` addresses.)

- `informal shell` printed an empty property table and exited 0 for a
  design it could not model (OpenTitan `prim_count`, until
  `set_lowering skip_initial_checks on` is given) and for a design whose
  `bind` is invalid (common_cells `cc_counter`).
- The shell does not check immediate assertions (`assert (...)` in a
  clocked or combinational block) and said nothing about them. Every
  SymbiYosys example and three of the common_cells proofs are written
  that way.
- Immediate assertions without a label are still not checked (reported
  NOT CHECKED): 9 of the 14 SymbiYosys examples and common_cells `lzc`,
  `ecc` and `heaviside`, which SymbiYosys proves, get no verdict.
- An unbounded `always` inside a property (`always s_eventually p`, the
  EBMC PWM examples) is not supported.
- svlang accepts the `bind` of common_cells `cc_fifo`, which slang rejects
  when run to the letter of the standard.

## Changes made because of this comparison

- InFormal `98b4b81` ("No silent empty property table"): a design that
  cannot be modelled is reported NOT MODELLED and `prove` fails; labelled
  immediate assertions and covers are checked; unlabelled ones are listed
  NOT CHECKED. It was written after the first pass of this comparison
  showed the empty tables. `results/before-fix-6dde4a5.jsonl` is that
  first pass on the hand-listed cases, kept so that the effect can be
  seen: on the SymbiYosys examples InFormal went from 1 property decided
  to 37, and OpenTitan `prim_count` from none to 14. No engine was changed.

In the cases: the OpenTitan designs set `set_lowering skip_initial_checks
on` for InFormal (a documented switch, off by default), because their
`ASSERT_INIT_NET` checks are otherwise reported as not modelled.
