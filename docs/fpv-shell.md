# FPV shell and the JasperGold / VC Formal compatibility layer

`informal shell` runs FPV setup scripts written for JasperGold (`jg`) or VC
Formal (`vcf`) without rewriting them. Scripts run in a real Tcl 8.6
interpreter (the C library, linked through FFI in `informal-tcl`), so `proc`,
`foreach`, `source`, namespaces and `[info script]` behave as they do in
the vendor tools. Only the tool commands are informal's.

```sh
informal shell tests/fpv/counter/counter.jg.tcl          # flavor detected from the script
informal shell setup.tcl --flavor vcf --workdir run1     # explicit flavor and scratch dir
informal shell --flavor jg                               # interactive prompt
informal shell --flavor jg -c 'analyze -sv a.sv' -c 'elaborate -top a'
```

Needs the engines in `config/engines.toml`; the SystemVerilog front end
(svlang) is built in. Generated files go to the work
directory (`informal_work/` by default).

## How a prove runs

1. `analyze` / `read_file` collect files, `+define+`, `+incdir+` and `-f`
   filelists (paths resolve the way VCS does; `$VAR` expands).
2. `elaborate` runs svlang over the design (decision D14): SystemVerilog
   errors come back with file, line and column, the top module comes from
   it, and from then on every property is checked against the names the top
   module declares, so a typo is an error rather than an implicit net that
   makes the check vacuous. The `reset` expression and `clock` names are
   checked the same way.
3. The models are built by svlang: it lowers the design (with the property
   harness injected) to a word-level netlist, and `crates/informal-svlang`
   writes it as Btor2. Nothing falls back: a SystemVerilog error in the
   harness, a construct svlang does not handle yet, and a construct it does
   not model (`$isunknown`, latches, blocks on different clocks or edges,
   the same bits assigned in two always blocks) make the property an error,
   naming the construct. A wrong guess there would be a wrong verdict.
   `tests/sv` holds small designs with known answers for this.
   `INFORMAL_FRONTEND=slang` uses the older slang lowering instead, as a
   cross-check. `informal lower design.sv -D NAME` prints the Btor2 for a
   design whose labelled immediate assertions become bad states.
4. Each property is lowered by the SVA subset compiler (`informal-fpv/src/sva.rs`)
   into an immediate assertion over `$past`, and the checker block is
   inserted before the top module's `endmodule`, so properties see every
   signal in the top module's scope (not yet signals inside sub-instances).
5. Reset: the reset condition holds in cycle 0 and is released after, as
   both vendor tools do by default. Checks start once the property's whole
   window lies after reset.
6. Each assertion and cover becomes its own Btor2 model, with every
   assumption as a constraint, and runs through the engine portfolio with
   the session time limit. A cover is checked as "never matches": a
   counterexample means covered, a proof means unreachable.

## Property source locations

Each property in the session JSON carries `source: {file, line}` (or
`null`): the absolute script path and 1-based line of the Tcl command that
created it, from Tcl's `info frame` (`informal_tcl::caller_location`). Tcl 8.6
tracks lines for code read by `source`, including `foreach` and proc bodies
written in that file. A property made by `eval` of a built string has no
line info; then the first line of the sourced script (`info script`) naming
the property as a whole word is used. Properties typed at the console get
`null`. SVA written in the RTL is not read by the shell yet, so no RTL
locations are produced today; the field is ready for them. The app's source
view opens `source.file` at `source.line` (F3, Show source, the Details
Source row) and only falls back to searching the top design file when it is
absent.

## SVA subset

Supported: `@(posedge clk)` / `@(negedge clk)`, `disable iff (expr)`,
sequences of boolean terms joined by fixed delays (`a ##1 b ##2 c`, a
leading `##N`), `|->` and `|=>`, and any boolean SystemVerilog expression
inside a term, including `$past(x[, n])`, `$rose`, `$fell`, `$stable`,
`$changed`.

Rejected with an error naming the construct: `##[m:n]` ranges, `[*n]`,
`[->n]`, `[=n]`, `throughout`, `within`, `intersect`, `first_match`,
`until`, `s_eventually` and other liveness operators, `not`, `strong` /
`weak`. These come with the slang-based SVA compiler later in phase 1.

## Command map

Options that only tune vendor engines, proof grids or the GUI are accepted
and ignored with a note on stderr. Options that would change what is
checked and are not implemented are errors, so a migrated script never
passes on a different setup without saying so.

| JasperGold | VC Formal | informal behaviour |
| --- | --- | --- |
| `clear -all` | (new session) | Forget design and properties |
| `analyze -sv12 [-f F] [+define+X] [+incdir+D] files` | `analyze -format sverilog [-vcs {...}] files` | Collect sources; VHDL is an error for now |
| `elaborate -top T [-parameter P V]` | `elaborate T [-vcs {-pvalue+P=V}]`, `read_file -top T -vcs {...}` | Elaborate with svlang; top guessed when omitted |
| `elaborate ... -bbox_m {M ...} -bbox_i {path ...}` | `set_blackbox -designs {M ...} -cells {path ...}` | Black boxes: ports kept, contents not modelled, outputs free (see below) |
| `stopat sig ...`, `stopat -clear [sig ...]`, `stopat -list` | `snip_driver sig ...` | Cut points: each signal is cut from its drivers and is free on every step (see below) |
| `clock clk` | `create_clock clk -period N` | Primary clock (posedge). `-both_edges`, `-factor` are errors |
| `reset -expression {!rst_n}` | `create_reset rst_n -sense low` | Reset held in cycle 0, released after |
| — | `sim_run -stable`, `sim_save_reset` | Accepted (reset is applied by the model) |
| `assert / assume / cover -name N {expr}` | `fvassert / fvassume / fvcover N -expr {expr}` | Add a property |
| `assert -env` | — | Add as assumption |
| `assert -disable/-enable pattern` | `fvdisable / fvenable pattern` | Glob patterns |
| `prove -all`, `prove -property {names}` | `check_fv [-property {names}]` | Prove; prints the report, returns a count summary |
| `set_prove_time_limit 1h` | `set_fml_var fml_max_time 1H` | Per-property time limit |
| `set_engine_mode {Ht B N}` | — | Mapped to informal engines (Ht/Hp → `ric3-ic3`, `abc-pdr`; B → `ric3-bmc`; N → `ric3`) |
| `get_property_list -include {type assert status proven}` | `get_props -type assert -status proven` | Tcl list of names |
| `get_status name` | — | `proven`, `cex`, `covered`, `unreachable`, `undetermined`, `error`, `unprocessed` |
| `report` | `report_fv -list` | Property table |
| `include file.tcl` | `source file.tcl` | Source a script |
| `clock seconds` etc. | — | Forwarded to Tcl's own `clock` |

Extra informal commands in every flavor: `informal::session` (the session as JSON)
and `informal::set_engines name ...` (pick engines by name). Each flavor's
commands also stay reachable by namespace (`jg::prove`, `vcf::check_fv`).

## Cut points (`stopat`)

`stopat u_core.busy` cuts the signal from whatever drives it (an assign, a
register, an instance output): everything that reads it sees a free value
on every step. Names are paths from the top module; a name that is not a
signal of the design is an error. `stopat -clear` removes them again.

A cut is an over-approximation the user asks for, usually to take a deep
counter, a memory or a whole sub-block out of the proof:

- a property **proved** with cut points holds on the real design;
- a **counterexample** may use values the design cannot produce at the cut
  signal. The trace shows the cut signal as an input, and the prove log
  lists the cut points in force.

The signals that drive the cut one, and signals it merely copied, are not
affected. Later reads of a variable inside the same procedural block that
assigned it see the block's own value, not the cut. svlang applies the cut
(`LowerConfig::cut_points`); the slang cross-check lowering rejects it.

## Black boxes

`elaborate -top T -bbox_m {ram fifo}` (VC Formal: `set_blackbox -designs
{ram fifo}` before `read_file` / `elaborate`) makes every instance of the
named modules a black box: its ports stay, nothing inside it is part of
the model (no registers, memories, child instances or assertions), and its
outputs are free on every step. `-bbox_i u_core.u_mem` (`set_blackbox
-cells`) does the same for one instance by path.

It is the same over-approximation as a cut point, taken at a module
boundary, and it also saves the work of lowering the module: a proof
holds for the design, a counterexample may use output values the boxed
logic cannot produce. The prove log lists the boxes in force. A module
name with no instance in this design is ignored with a note (flow scripts
list boxes for every configuration); an instance path that does not exist
is an error. Signals inside a box cannot be named in properties.

## Checking a script before running it

`informal shell --check script.tcl` goes through a JasperGold or VC Formal
script without elaborating or proving anything, and lists per line what
informal would do with it:

```
fpv.tcl:85: unknown command: get_version: not an informal command
fpv.tcl:135: error: clock: -both_edges is not supported yet
fpv.tcl:238: ignored: task -create ...: tasks are not modelled yet; properties stay in one task
fpv.tcl:45: tcl error: environment variable DUT_TOP is not set
9 command(s) honoured, 5 rejected, 2 unknown, 4 ignored option(s) or command(s), 1 Tcl error(s)
```

- `error`: informal has the command but rejects this use; a real run stops here.
- `unknown command`: not implemented.
- `ignored`: accepted, with no effect.
- `tcl error`: anything else (an unset variable, a missing file).

The script is evaluated one top-level command at a time, so one error does
not hide the rest; `source` and `include` are followed. Design files need
not exist. An environment variable that is not set is reported once and
given a placeholder. The check has no side effects: `exec`, `cd`, writing
or deleting files and the script's own `puts` are not done (this guards
against what a flow script does in passing; it is not a sandbox for a
hostile script). `--json` prints the report for tools; the exit status is 1
when anything but `ignored` was found.

`scripts/vendor_tcl_corpus.py` fetches public vendor scripts and tallies
these findings over all of them; `docs/vendor-script-tally.md` holds the
last result, the ranked list of what the shell should learn next.

## Building with slang (optional cross-check)

slang is found in `INFORMAL_SLANG_DIR` or `$INFORMAL_TOOLS_DIR/slang` (a
checkout built with `cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
-DCMAKE_POSITION_INDEPENDENT_CODE=ON && cmake --build build`). The C++
shim in `crates/informal-slang/cpp` is compiled against it. Without slang, informal
builds as usual; only `INFORMAL_FRONTEND=slang` is unavailable.

## Known gaps

- Properties can name signals in sub-instances (`u_wp.p`). No `bind` yet.
- Concurrent properties written in the RTL (`assert property`, `assume
  property`, `cover property` inside a module) are found at `elaborate`
  and listed under their full hierarchical names (`top.u_fifo.a_full`,
  `top._assert_1` without a label); `prove -all` checks them with the
  rest, and an assumption constrains every proof. svlang compiles the
  fixed-length ones: booleans, `##N`, `[*N]`, `|->`, `|=>`, `disable iff`,
  `default clocking` and the sampled-value functions. An assertion or
  cover that uses another operator (`##[m:n]`, `until`, `s_eventually`,
  ...) gets the status error naming the operator; an assumption that does
  is an error for the whole design. A concurrent assertion under an `if`
  or `case` in a clocked block is checked only when the flow reaches it
  (`if (en) assert property (p)` is `en |-> p`); one inside a loop or a
  task is not supported yet.
- One clock. A design that uses one edge of it takes one model step per
  clock cycle. A design that uses both edges (`posedge` and `negedge`
  blocks, or a property on the other edge) takes two: the clock, which
  must be a top-level input, starts low and toggles on every step, each
  register changes only at its own edge, the reset is held for the whole
  first cycle, and the other inputs may change on either edge. Depths
  are then in half cycles, and the trace shows the clock toggling. A
  design with two clocks, or both edges of a clock that is not a
  top-level input, is an error.
- A latch (`always_latch`, or an `always @*` that does not assign on
  every path) is transparent within a model step: open, it shows its
  data; closed, what it held at the end of the step before. A latch
  whose value depends on itself within a step, through its own data or
  through another latch (a two-phase pair), is an error. A latch opened
  by the clock's level needs the clock as data, which the one-edge model
  leaves free.
- `$past`, `$rose`, `$fell`, `$stable` and `$changed` have no history in
  the first cycle(s). As in JasperGold, a property that uses them is first
  checked once that history is real: with a reset the reset cycle counts,
  so checks start right after it; without one they start a cycle (or the
  `$past` depth) later.
- Values are two-state: X and Z read as 0, `$isunknown` is an error, an
  out-of-range read is a free value.
- `initial` blocks: constant assignments to whole registers give their
  starting values; anything else in them is ignored with a note.
- Black boxes need the boxed module's source to elaborate (a module with
  no source at all is still an error); `-bbox_mul` and `-bbox_a` (box
  wide multipliers and arrays automatically) are ignored with a note.
- Tasks (`task -create`) are not modelled; everything runs in one task, so
  `assume -task T` is an error (it would constrain every property).
