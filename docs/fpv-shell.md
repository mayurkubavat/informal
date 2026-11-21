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
   to prove". Every procedural immediate assertion and cover in the RTL
   (labelled, unlabelled, across multiple instances, or unrolled across
   loops) is listed under its unique hierarchical name and proved like
   any concurrent property, with vacuity covers on conditional checks.
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
`accept_on` / `reject_on` / `sync_accept_on` / `sync_reject_on`,
booleans joined by delays (`a ##1 b ##2 c`, a leading `##N`, ranges
`##[M:N]` and `##[M:$]`), consecutive (`[*N]`, `[*M:N]`, `[*M:$]`), goto
(`[->N]`, `[->M:N]`) and non-consecutive (`[=N]`, `[=M:N]`) repetition,
sequence `and`, `or`, `intersect`, `within`, `throughout` and
`first_match`, `|->`, `|=>`, `#-#` and `#=#`, property `and`, `or`,
`not`, `implies`, `iff` and `if (b) p1 [else p2]`, `b until c` /
`b until_with c` and their strong forms `s_until` / `s_until_with`,
`s_eventually b`, `nexttime` / `s_nexttime`, `eventually`, bounded and
compound unbounded `always` (`nexttime [2] p`, `s_eventually [1:3] b`,
`always [0:2] b`, `start |=> always p`), procedural concurrent assertions
inside static loops and tasks, and any pure RTL SystemVerilog expression
inside (including module/package functions, `$past(x[, n])`, `$rose`,
`$fell`, `$stable`, `$changed`).

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

A property given by command is bound at `prove`. Names not declared in
the top module's scope (signals, child instances, module/package functions,
sequences, properties, parameters, or typedefs) are rejected at the command
itself so typos never become implicit nets; any unsupported SVA construct
is reported on that property at `prove`.

## Command map

Options that only tune vendor engines, proof grids or the GUI are accepted
and ignored with a note on stderr. Options that would change what is
checked and are not implemented are errors, so a migrated script never
passes on a different setup without saying so.

| JasperGold | VC Formal | informal behaviour |
| --- | --- | --- |
| `clear -all` | (new session) | Forget design and properties |
| `analyze -sv12 [-f F] [+define+X] [+incdir+D] [-bbox_m {M ...}] files` | `analyze -format sverilog [-vcs {...}] files` | Collect sources (and optional black-box modules); VHDL is an error for now |
| `elaborate -top T [-parameter P V]` | `elaborate T [-vcs {-pvalue+P=V}]`, `read_file -top T -vcs {...}` | Elaborate with svlang; top guessed when omitted |
| `elaborate ... -no_preconditions`, `-create_related_covers {precondition witness}` | — | Leave out, or ask for, precondition vacuity covers and witness covers (`<name>:witness` VCD traces) run after every prove (docs/vacuity.md) |
| `elaborate ... -bbox_m {M ...} -bbox_i {path ...} [-bbox_mul W] [-bbox_a N]` | `set_blackbox -designs {M ...} -cells {path ...}` | Black boxes (with or without RTL source) and complexity auto-boxing of wide multipliers (`>= W` bits) and memories (`>= N` words); outputs are free cuts (see below) |
| `stopat sig ...`, `stopat -clear [sig ...]`, `stopat -list` | `snip_driver sig ...` | Cut points: each signal is cut from its drivers and is free on every step (see below) |
| `clock clk [-factor N] [-phase P]` | `create_clock clk -period N` | A clock (posedge); the first one declared is the primary clock. `-factor` and `-phase` relate several clocks (see Known gaps); `-both_edges` is accepted and not needed |
| `reset -expression {!rst_n}` (or `-expr`) | `create_reset rst_n -sense low` | Reset held in cycle 0, released after |
| — | `sim_run -stable`, `sim_save_reset` | Accepted (reset is applied by the model) |
| `assert / assume / cover -name N [-bound K] {expr}` | `fvassert / fvassume / fvcover N -expr {expr}` | Add a property (`-bound K` caps assertions/covers to `K` steps, reporting `bounded_proven` if unviolated up to `K`) |
| `assert -env` | — | Add as assumption |
| `assert -disable/-enable pattern` | `fvdisable / fvenable pattern` | Glob patterns. A property of the RTL can be named without its path (`a_never` for `top.a_never`) when that names one property |
| `assert -disable/-enable/-remove -regexp {re}` | — | Regular expressions, matched anywhere in the name as Tcl's `regexp` does (anchor with `^` and `$`) |
| `prove -all [-bound K] [-with_proof_core]`, `prove -property {names}`, `prove -task T` | `check_fv [-property {names}]` | Prove; prints the report, returns a count summary. `-bound K` caps BMC depth; `-with_proof_core` extracts minimal needed assumptions and state support |
| `set_prove_time_limit 1h`, `set_prove_per_property_time_limit 1h` | `set_fml_var fml_max_time 1H` | Per-property time limit; units `ms`, `s`, `m`, `h`, `d` |
| `set_engine_mode {Ht B N}` | — | Mapped to informal engines (Ht/Hp → `ric3-ic3`, `abc-pdr`; B → `ric3-bmc`; N → `ric3`); prints the engines it picked |
| `get_property_list [-task T] -include {type assert status proven}` | `get_props -type assert -status proven` | Tcl list of names; with `-regexp`, a `name` value is a regular expression. Keys: `type`, `status`, `name`, `disabled` (0 or 1), `task` |
| `get_status name` | — | `proven`, `bounded_proven`, `cex`, `covered`, `unreachable`, `undetermined`, `error`, `unprocessed` |
| `report [-task T] [-csv] [-file F [-force]]` | `report_fv -list` | Property table, or comma-separated values; `-file` writes it there; `-task` keeps one task's properties |
| `task -create T ...`, `task -edit T ...`, `task -set T`, `task -list`, `get_task_list`, `task -delete T` | — | Tasks: see below |
| `get_design_info [-json]`, `report -design` | — | What the design holds and the size of the current task's model: see Design and proof statistics |
| `get_design_info -list instance\|memory\|clock\|reset [-filter P [-regexp]]` | — | Names of instances, memories, clocks or asynchronous resets found in the design |
| `get_property_info P [-json] [-cone] [-proof_core]`, `report -detail [-property P] [-task T]`, `get_stats [-task T]` | — | What the last prove measured per property (plus COI or minimal needed-assumption proof core): see Design and proof statistics |
| `assert/assume/cover -task T ...`, `stopat -task T ...` | — | Add to task T instead of the current task |
| `visualize -violation -property P`, `visualize -cover -property P` | — | Returns the VCD that prove wrote for a CEX or a cover, and opens it with `$INFORMAL_WAVE_VIEWER` (for example `surfer` or `gtkwave`) when that is set |
| `get_version`, `echo words` | `get_version`, `echo words` | The informal version; print a line |
| `check_assumptions [-depth N] [-conflict] [-dead_end]` | — | Whether the assumptions and reset allow traces of N steps (`-conflict` returns minimal conflicting assumptions; `-dead_end` runs 2QBF trap-state detection) |
| `set_prove_cache [on\|off]`, `set_prove_cache_path [dir]`, `set_prove_cache_mode [read_write\|read_only\|write_only]` | `set_prove_cache ...` | Content-addressed SHA-256 proof cache of `proven` and `unreachable` verdicts across runs |
| `check_conn -load <csv> [-task T]`, `check_conn -prove`, `check_conn -report`, `check_conn -list` | `check_conn ...` | Formal connectivity verification from OpenTitan/JasperGold `CONNECTION`+`CONDITION`+`LATENCY` CSVs into task `Connectivity` (or `-task T`) |
| `check_cov -init [-model ...]`, `check_cov -waiver -add ...`, `check_cov -measure`, `check_cov -report [-html] [-report_file F]` | `check_cov ...` | Formal COI observability coverage, branch/toggle UNR dead-code detection, functional covers, and waivers |
| `check_xprop -init [-targets ...]`, `check_xprop -waiver -add ...`, `check_xprop -prove`, `check_xprop -report` | `check_xprop ...` | Exact 2-copy self-composition X-propagation verification over reset-less flops, X literals, and undriven nets |
| `check_spv -create -from <S> -to <O> [-cond <C>]`, `check_spv -prove`, `check_spv -report`, `check_spv -list` | `check_spv ...` | 2-safety non-interference / security path taint verification catching direct and multi-cycle covert storage channels |
| `check_sec -setup -spec <F> -imp <F> [-lec] [-latency N]`, `check_sec -map ...`, `check_sec -prove`, `check_sec -report` | `check_sec ...` | Dual-design formal sequential (`SEC`, with optional retiming `-latency N`) and combinational keypoint (`-lec`) equivalence checking with side-by-side `spec`/`imp` VCD traces |
| `assume -from_assert <P> [-name N]` | `fvassume -from_assert <P>` | Promote proven/helper assertion(s) into environment assumptions with provenance tracking (never circularly assuming `<P>` when proving `<P>`) |
| `get_fv_complexity [-property P] [-json]`, `blackbox_assistant [-threshold_bits B] [-apply] [-json]` | `get_fv_complexity ...`, `blackbox_assistant ...` | Cone-of-influence flop/memory/wide-arithmetic complexity breakdown and boxing recommender |
| `check_fv_setup [-json]`, `report_fv_setup`, `get_reset_info [-json]` | `check_fv_setup ...` | Audit formal setup completeness (clocks, resets, reset-less flops, undriven nets, abstractions, helper assumptions) |
| `abstract -counter <sig> [-threshold K]`, `report_abstractions`, `set_abstractions -clear` | `abstract ...` | Sound over-approximate counter jump abstraction fast-forwarding wide counters to `K - 1` |
| `why_cex [-property P] [-max_events N] [-json] [<P>]` | `why_cex ...` | Dynamic backward causal root-cause slice of a counterexample or witness trace (pruning untaken mux arms and non-controlling operands) |
| `include file.tcl` | `source file.tcl` | Source a script |
| `clock seconds` etc. | — | Forwarded to Tcl's own `clock` |

Extra informal commands in every flavor: `informal::session` (the session as JSON),
`informal::set_engines name ...` (pick engines by name) and
`informal::set_vacuity on|off` (the vacuity checks before and after a prove; on by default). Each flavor's
commands also stay reachable by namespace (`jg::prove`, `vcf::check_fv`).

## Tasks

A task is a named set of properties, with its own assumptions and cut
points, as in JasperGold. Every session starts with the task
`<embedded>`, which holds the properties written in the RTL and every
property given by command until another task is made current. Decision
D20 in docs/decisions.md says why it works this way.

| Command | What it does |
| --- | --- |
| `task -create T [copy options] [-set]` | Make task T, empty apart from the copies asked for; `-set` makes it current. Returns T |
| `task -edit T [copy options]` | Copy more properties into T |
| `task -set T` | Make T the current task: commands without `-task` add to it, `prove -all` proves it, and a short name is looked up in it |
| `task` | The current task's name |
| `task -list`, `get_task_list` | The task names, `<embedded>` first |
| `task -delete T` | Remove T and its properties (not `<embedded>`); the current task goes back to `<embedded>` when it was T |

Copy options take the properties of the source task: `-source_task S`,
else the current task.

- `-copy {names}`: by name or glob (`*`, `?`), matched against the full
  name and the name without its task; with `-regexp`, regular expressions.
- `-copy_asserts`, `-copy_assumes`, `-copy_covers`: every one of that kind;
  `-copy_all` all three and the cut points.
- `-copy_stopats`: the source task's cut points.
- `-copy_ratings`, `-copy_abstractions`, `-copy_related_covers` and
  `-copy_proof_settings` are accepted and ignored with a note.

A property outside `<embedded>` is named `T::name`, both when it is made
there (`assert -task T -name a_x ...` or with T current) and when it is
copied (`T::a_x`); an embedded property also answers to
`<embedded>::name`, as JasperGold scripts write it. A copy starts
unprocessed and enabled, whatever the original's state.

Each task is proved on a model of its own: the design, the task's
enabled assumptions and its cut points, nothing of another task. So an
assumption of one task does not constrain another, and a cut point in
one task leaves the same signal driven in the others. Black boxes,
clocks and reset are the design's and the same in every task.

Assumptions written in the RTL are part of the design and are copied
into every new task from the start so that its property list says what
its model holds; disabling (`assume -disable`) or removing (`assume
-remove`) an RTL assumption in a task leaves its constraint out of that
task's model while keeping it active in other tasks.

```tcl
task -create FreeFsm -copy {a_fsm_*} -copy_assumes
stopat -task FreeFsm u_fsm.state_q
prove -task FreeFsm
report -task FreeFsm
```

## Design and proof statistics

Every number below is counted on the design or measured by a prove; none
is estimated. What was not measured is said so ("not measured", "not
checked"), and in JSON the field is left out.

`get_design_info` (also `report -design`) prints a table; `-json` gives
the same as JSON (the app's Design Info panel and the MCP `design_info`
tool read that):

| Row | Counted on |
| --- | --- |
| instances, inputs, outputs, flops, latches, memories (words x width), clocks and asynchronous resets found, undriven ranges | svlang's netlist of the design as elaborated: before black boxes, cut points and properties. Flops are registers written on a clock edge, latches the level-sensitive ones; a memory is an unpacked array as declared (one of up to 4096 words still becomes registers in the model). A clock counts the registers and memory write ports on it |
| clocks and reset declared, black boxes | the script |
| task, cut points, properties | the current task: its assertions, covers, enabled assumptions (those of the design included) and disabled properties |
| model nodes, states, inputs, targets | the current task's Btor2 model as the engines get it (built for the command, so it takes one lowering): every line but sorts, `state` and `input` lines with their bits, `bad`, `justice` and `constraint` lines |
| AIG | that model bit-blasted as the in-house engines load it: and gates, latches, inputs |

`get_property_info P` (and `report -detail`, one block per property;
`get_stats` gives every property's as JSON):

| Field | Meaning |
| --- | --- |
| engine | the engine whose verdict was taken |
| wall time | the property's portfolio run |
| cpu time | user and system time of the process and of the engine processes it waited for while the property ran; the engines run in parallel, so it can exceed the wall time |
| peak memory | the resident high-water mark of the process or an engine process, given only when it rose during this property's run (so it is this run's peak) |
| depth | `trace`: cycles of the counterexample or cover trace; `k`: the k of a k-induction proof; `frames`: IC3/PDR frames when the winner found its invariant; `bound`: the BMC bound an undetermined run reached |
| deepest bound | the deepest bound any engine reported |
| invariant clauses | clauses of the inductive invariant the winner gave (its lemmas) |
| evidence | what the winner gave: `invariant`, `k-induction`, `two-step`, `two-step-k-induction`, `trace`; "none given" for external engines |
| certificate | the file written for an outside checker (`informal shell --certificate`), or why none was; "not asked for" otherwise |
| vacuity | what the vacuity checks found (docs/vacuity.md); "not checked" for a proven assertion they did not look at |
| cone of influence | with `-cone`: the states and inputs (and their bits) the property's target and the assumptions depend on in its model |

The same fields are in the session JSON (`stats` of each property) and
the MCP replies.

## Cut points (`stopat`)

`stopat u_core.busy` cuts the signal from whatever drives it (an assign, a
register, an instance output): everything that reads it sees a free value
on every step. Names are paths from the top module; a name that is not a
signal of the design is an error. `stopat -clear` removes them again.
Cut points belong to a task (`stopat -task T`, else the current task):
see Tasks above.

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
is an error. Signals inside a box cannot be named in properties, and the
assertions written inside a boxed module stay in the list with the
status `error` ("not in the model (inside a black box)") from elaborate
on, which says which they are.

## Checking a script before running it

`informal shell --check script.tcl` goes through a JasperGold or VC Formal
script without elaborating or proving anything, and lists per line what
informal would do with it:

```
fpv.tcl:85: unknown command: check_cov: not an informal command
fpv.tcl:135: error: analyze: VHDL is not supported yet
fpv.tcl:241: ignored: prove -orchestration: ignoring -orchestration (vendor engine/GUI option)
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
- Source-less black boxes (`-bbox_m` when the module's `.sv` source is absent)
  infer port direction from parent signal drivers (undriven connected signals
  become free cuts; wildcard `.*` connections on source-less stubs require a
  module header).
- Tasks: `prove -bg` runs in the foreground, so tasks are proved one
  after the other.
