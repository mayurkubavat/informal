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

Needs Yosys (`yosys` on `PATH`, `INFORMAL_YOSYS`, or `/tools/yosys/current/bin/yosys`)
and the engines in `config/engines.toml`. Generated files go to the work
directory (`informal_work/` by default).

## How a prove runs

1. `analyze` / `read_file` collect files, `+define+`, `+incdir+` and `-f`
   filelists (paths resolve the way VCS does; `$VAR` expands).
2. `elaborate` runs slang over the design (when informal is built with it):
   SystemVerilog errors come back with file, line and column, the top
   module comes from slang, and from then on every property is checked
   against the names the top module declares, so a typo is an error rather
   than an implicit net that makes the check vacuous. The `reset`
   expression and `clock` names are checked the same way. It then runs
   Yosys once only when Yosys is the front end (no slang, or
   `INFORMAL_FRONTEND=yosys`).
3. The models are built by slang's AST lowering (`crates/informal-slang/cpp/lower.cpp`)
   when informal has slang, else by Yosys. `INFORMAL_FRONTEND=yosys` forces Yosys,
   and a design using something the lowering does not handle yet (memories,
   loops, functions, interfaces, latches) falls back to Yosys with a note.
   Nothing else falls back: a SystemVerilog error in the harness, and a
   construct neither front end models (`$isunknown`, blocks on different
   clocks or edges, the same bits assigned in two always blocks), make
   the property an error. A wrong guess there would be a wrong verdict.
   `tests/sv` holds small designs with known answers for this. Both give
   the same verdicts on `tests/fpv`; counterexample depths from the slang
   path are one cycle shorter, since Yosys registers clocked assertions.
   `informal lower design.sv -D NAME` prints the slang Btor2 for a design whose
   labelled immediate assertions become bad states.
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
| `elaborate -top T [-parameter P V]` | `elaborate T [-vcs {-pvalue+P=V}]`, `read_file -top T -vcs {...}` | Elaborate with slang (and Yosys); top guessed when omitted |
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

## Building with slang

slang is found in `INFORMAL_SLANG_DIR` or `~/Silicon/DV/tools/slang` (a
checkout built with `cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
-DCMAKE_POSITION_INDEPENDENT_CODE=ON && cmake --build build`). The C++
shim in `crates/informal-slang/cpp` is compiled against it. Without slang, informal
still builds and falls back to a regex guess of the top module.

## Known gaps

- With the slang front end, properties can name signals in sub-instances
  (`u_wp.p`); with Yosys, only signals in the top module. No `bind` yet.
- One clock and one edge; every register steps on each model step. A
  design with blocks on two clocks or both edges is an error.
- `$past`, `$rose`, `$fell`, `$stable` and `$changed` have no history in
  the first cycle(s). As in JasperGold, a property that uses them is first
  checked once that history is real: with a reset the reset cycle counts,
  so checks start right after it; without one they start a cycle (or the
  `$past` depth) later.
- Values are two-state: X and Z read as 0, `$isunknown` is an error, an
  out-of-range read is a free value.
- `initial` blocks: constant assignments to whole registers give their
  starting values; anything else in them is ignored with a note.
- `-bbox_m` is recorded but not applied (the full design is proved).
- Tasks (`task -create`) are not modelled; everything runs in one task, so
  `assume -task T` is an error (it would constrain every property).
