# FPV shell and the JasperGold / VC Formal compatibility layer

`fdv shell` runs FPV setup scripts written for JasperGold (`jg`) or VC
Formal (`vcf`) without rewriting them. Scripts run in a real Tcl 8.6
interpreter (the C library, linked through FFI in `fdv-tcl`), so `proc`,
`foreach`, `source`, namespaces and `[info script]` behave as they do in
the vendor tools. Only the tool commands are fdv's.

```sh
fdv shell tests/fpv/counter/counter.jg.tcl          # flavor detected from the script
fdv shell setup.tcl --flavor vcf --workdir run1     # explicit flavor and scratch dir
fdv shell --flavor jg                               # interactive prompt
fdv shell --flavor jg -c 'analyze -sv a.sv' -c 'elaborate -top a'
```

Needs Yosys (`yosys` on `PATH`, `FDV_YOSYS`, or `/tools/yosys/current/bin/yosys`)
and the engines in `config/engines.toml`. Generated files go to the work
directory (`fdv_work/` by default).

## How a prove runs

1. `analyze` / `read_file` collect files, `+define+`, `+incdir+` and `-f`
   filelists (paths resolve the way VCS does; `$VAR` expands).
2. `elaborate` runs slang over the design (when fdv is built with it):
   SystemVerilog errors come back with file, line and column, the top
   module comes from slang, and from then on every property is checked
   against the names the top module declares, so a typo is an error rather
   than an implicit net that makes the check vacuous. It then runs Yosys
   once, which builds the model.
3. The models are built by slang's AST lowering (`crates/fdv-slang/cpp/lower.cpp`)
   when fdv has slang, else by Yosys. `FDV_FRONTEND=yosys` forces Yosys,
   and a design using something the lowering does not handle yet (memories,
   loops, functions, interfaces) falls back to Yosys with a note. Both give
   the same verdicts on `tests/fpv`; counterexample depths from the slang
   path are one cycle shorter, since Yosys registers clocked assertions.
   `fdv lower design.sv -D NAME` prints the slang Btor2 for a design whose
   labelled immediate assertions become bad states.
4. Each property is lowered by the SVA subset compiler (`fdv-fpv/src/sva.rs`)
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
created it, from Tcl's `info frame` (`fdv_tcl::caller_location`). Tcl 8.6
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

| JasperGold | VC Formal | fdv behaviour |
| --- | --- | --- |
| `clear -all` | (new session) | Forget design and properties |
| `analyze -sv12 [-f F] [+define+X] [+incdir+D] files` | `analyze -format sverilog [-vcs {...}] files` | Collect sources; VHDL is an error for now |
| `elaborate -top T [-parameter P V]` | `elaborate T [-vcs {-pvalue+P=V}]`, `read_file -top T -vcs {...}` | Elaborate with Yosys; top guessed when omitted |
| `clock clk` | `create_clock clk -period N` | Primary clock (posedge). `-both_edges`, `-factor` are errors |
| `reset -expression {!rst_n}` | `create_reset rst_n -sense low` | Reset held in cycle 0, released after |
| — | `sim_run -stable`, `sim_save_reset` | Accepted (reset is applied by the model) |
| `assert / assume / cover -name N {expr}` | `fvassert / fvassume / fvcover N -expr {expr}` | Add a property |
| `assert -env` | — | Add as assumption |
| `assert -disable/-enable pattern` | `fvdisable / fvenable pattern` | Glob patterns |
| `prove -all`, `prove -property {names}` | `check_fv [-property {names}]` | Prove; prints the report, returns a count summary |
| `set_prove_time_limit 1h` | `set_fml_var fml_max_time 1H` | Per-property time limit |
| `set_engine_mode {Ht B N}` | — | Mapped to fdv engines (Ht/Hp → `ric3-ic3`, `abc-pdr`; B → `ric3-bmc`; N → `ric3`) |
| `get_property_list -include {type assert status proven}` | `get_props -type assert -status proven` | Tcl list of names |
| `get_status name` | — | `proven`, `cex`, `covered`, `unreachable`, `undetermined`, `error`, `unprocessed` |
| `report` | `report_fv -list` | Property table |
| `include file.tcl` | `source file.tcl` | Source a script |
| `clock seconds` etc. | — | Forwarded to Tcl's own `clock` |

Extra fdv commands in every flavor: `fdv::session` (the session as JSON)
and `fdv::set_engines name ...` (pick engines by name). Each flavor's
commands also stay reachable by namespace (`jg::prove`, `vcf::check_fv`).

## Building with slang

slang is found in `FDV_SLANG_DIR` or `~/Silicon/DV/tools/slang` (a
checkout built with `cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
-DCMAKE_POSITION_INDEPENDENT_CODE=ON && cmake --build build`). The C++
shim in `crates/fdv-slang/cpp` is compiled against it. Without slang, fdv
still builds and falls back to a regex guess of the top module.

## Known gaps

- With the slang front end, properties can name signals in sub-instances
  (`u_wp.p`); with Yosys, only signals in the top module. No `bind` yet.
- One clock; every register steps on each model step.
- `-bbox_m` is recorded but not applied.
- No counterexample waveforms yet (they come with UI v1).
- Tasks (`task -create`) are not modelled; everything runs in one task.
