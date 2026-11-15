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
3. The models are built by svlang, the only front end: it lowers the
   design to a word-level netlist, and `crates/informal-svlang` writes it
   as Btor2. Nothing falls back: a construct svlang does not handle yet,
   and a construct it does not model (`$isunknown`, two clocks that are
   not declared, the same bits assigned in two always blocks) make the
   property an error, naming the construct. A wrong guess there would be a
   wrong verdict. `tests/sv` holds small designs with known answers for
   this. `informal lower design.sv -D NAME` prints the Btor2 for a design
   whose labelled immediate assertions become bad states.
   A design that does not lower has no model, so its own properties are
   not listed: `elaborate` says `NOT MODELLED` with the constructs, a
   property given by command gets them as its error, and a `prove` with
   nothing to run fails with them. An empty table never means "nothing
   to prove". An immediate assertion or cover in an always block that has
   a label of its own is listed under that label and proved like any
   property; one with no label, or with a label used twice, is reported
   at `elaborate` as `NOT CHECKED`, with its file and line.
4. Each property given by command is bound by svlang in the top module's
   scope, so it sees every signal there and in the instances below, and is
   compiled by the same property compiler as the properties written in
   the RTL. No source file is copied or edited.
5. Reset: the reset condition holds in cycle 0 and is released after, as
   both vendor tools do by default (longer in a model that drives the
   clocks, see Known gaps). It is part of the model. Checks start once the
   property's whole window lies after reset.
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
`null`. A property written in the RTL carries the place it stands in the
design, and its `expr` is its text as written there. The app's source view opens `source.file` at `source.line` (F3,
Show source, the Details Source row) and only falls back to searching the
top design file when it is absent.

## SVA subset

Properties given by command and properties written in the RTL go through
the same compiler in svlang.

Supported: `@(posedge clk)` / `@(negedge clk)`, `disable iff (expr)`,
booleans joined by delays (`a ##1 b ##2 c`, a leading `##N`, ranges
`##[M:N]`), repetition `[*N]` and `[*M:N]`, sequence `and`, `or`,
`intersect`, `within`, `throughout` and `first_match`, `|->` and `|=>`,
`b until c` / `b until_with c` between booleans and their strong forms
`s_until` / `s_until_with`, `s_eventually b`, `nexttime`, `eventually` and
`always` with a bound (`nexttime [2] b`, `s_eventually [1:3] b`, `always
[0:2] b`), and any boolean SystemVerilog expression inside, including `$past(x[, n])`,
`$rose`, `$fell`, `$stable`, `$changed`.

`s_eventually` is a liveness property: no finite trace violates it. (A
strong until is both: what has to hold meanwhile is a safety check, and
its end having to come is liveness; the property is proven when both
are.) Its
model line is a Btor2 `justice`, which informal's own engines check as a
safety property (liveness to safety, `informal_aig::l2s`: a shadow copy of
the state and a free choice of the step where a loop starts). `proven`
means no loop of states leaves the awaited condition out for ever; `cex`
is such a loop, and the trace's first comment says which frames repeat.
An `assume` of `s_eventually` is a fairness constraint for these
properties. External engines are not run on them.

A property given by command is bound at `prove`. Rejected at the command
itself, with an error naming the construct: a name the top module does
not have, and the operators the compiler does not take (`[->n]`, `[=n]`,
`not`, `accept_on`, `#-#`). A range that ends at `$`, and `always` with
no bound inside another operator, are errors on that property at `prove`.

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
| `clock clk [-factor N] [-phase P]` | `create_clock clk -period N` | A clock (posedge); the first one declared is the primary clock. `-factor` and `-phase` relate several clocks (see Known gaps); `-both_edges` is accepted and not needed |
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
(`LowerConfig::cut_points`).

## Constructs left out on request

Two constructs that svlang reports as not supported can be left out of
the model, each by its own switch. Both are off unless set, so nothing is
skipped silently, and every `prove` logs the ones that are on.

```
set_lowering skip_initial_checks on
set_lowering isunknown_is_false on
set_lowering                          ;# shows both settings
```

- `skip_initial_checks`: an immediate assertion in an `initial` block
  that checks a signal at time zero (OpenTitan's `ASSERT_INIT_NET`) is
  left out. Only that check is not made; the design behaves the same.
- `isunknown_is_false`: `$isunknown(x)` is 0, since the model has no
  unknown value. A check such as `assert (!$isunknown(x))` then always
  holds, and says nothing about the design.

`informal lower` has the same two as `--skip-initial-checks` and
`--isunknown-is-false`. `clear -all` turns both off again.

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
fpv.tcl:135: error: analyze: VHDL is not supported yet
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

## Known gaps

- Properties can name signals in sub-instances (`u_wp.p`). No `bind` yet.
- Concurrent properties written in the RTL (`assert property`, `assume
  property`, `cover property` inside a module) are found at `elaborate`
  and listed under their full hierarchical names (`top.u_fifo.a_full`,
  `top._assert_1` without a label); `prove -all` checks them with the
  rest, and an assumption constrains every proof. svlang compiles the
  bounded ones: booleans, `##N`, `##[M:N]`, `[*N]`, `[*M:N]`, sequence
  `and`, `or`, `intersect`, `within`, `throughout` and `first_match`,
  under `|->` and `|=>`, with `disable iff`, `default clocking` and the
  sampled-value functions, `b until c` / `b until_with c` between
  booleans and their strong forms, and `s_eventually b` (liveness, see the
  SVA subset above). An assertion or cover that uses another operator
  (`##[1:$]`, `[->N]`, ...) gets the
  status error naming the operator; an assumption that does
  is an error for the whole design. A concurrent assertion under an `if`
  or `case` in a clocked block is checked only when the flow reaches it
  (`if (en) assert property (p)` is `en |-> p`); one inside a loop or a
  task is not supported yet.
- Clocks. A design that uses one edge of one clock takes one model step
  per clock cycle. A design that uses both edges (`posedge` and `negedge`
  blocks, or a property on the other edge), or more than one clock, is
  modelled with the clocks driven by the model: one model step is half a
  period of the fastest clock, every clock starts low, each register
  changes only at its own clock edge, and the other inputs may change on
  every step. Depths are then in model steps, and the trace shows the
  clocks toggling.
- Several clocks are declared with `clock`: `clock clk` and `clock clk2
  -factor 2` make `clk2` twice as slow as `clk`, and `-phase N` moves a
  clock's wave N model steps later (0 by default, so all clocks rise
  together for the first time). Every clock the design uses must be
  declared and be a top-level input. The reset is held until every
  declared clock has had a whole period.
- A register may be on a clock computed in the design (a gated clock,
  also through a clock-gating latch, or a divided one) once a clock is
  declared. Such a clock is read once in every model step, like any
  signal: the register takes the data of the step before when its clock
  has its edge between that step and this one. So a register on a
  divided clock sees the other registers as they were before the
  dividing edge (a simulator shows them after it), a plain `clk & en`
  gate opens with the value `en` has once clk is high, and an enable
  that changes while clk is high makes an edge. A property may be on
  such a clock too (`@(posedge gclk) ...`): it is checked on the values
  the edge sampled, and `$past` counts that clock's edges. A memory
  written on a computed clock works when it has up to 4096 words (its
  words are registers in the model); a larger one is an error.
  Properties given by command are on the first declared clock unless
  they name their own (`@(posedge clk2) ...`).
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
