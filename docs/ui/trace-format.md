# Counterexample and cover traces: witness to VCD

Status: implemented 2026-11-08: the survey (T0) and plan below, now built
(T2). Code: `named_witness` in `crates/informal-bmc/src/lib.rs`,
`crates/informal-fpv/src/trace.rs`, `Session::write_trace`, and the app's
`read_trace` command. Tests: `crates/informal-fpv/tests/trace.rs`, `flow.rs`, and
`trace.rs` unit tests.

## What exists today

| Piece | Has a witness? | Notes |
|---|---|---|
| `informal_engine::EngineResult` | field `witness: Option<String>` | Free text. Only `informal-engines-ext` fills it. `informal-cli check` prints it. Nothing else reads it. |
| External engines (`informal-engines-ext`) | with `cex_witness = true` (rIC3, ABC) or `witness_on_stdout = true` (pono, btormc) | `cex_witness`: the engine prints its counterexample in the witness format of the model it read (rIC3 `check --cex`: Btor2 witness on `.btor2`, AIGER witness on AIGER; ABC `write_cex -a -t /dev/stdout`: AIGER witness). `informal_engine::witness` converts it to `informalw 1` by position: Btor2 `state`/`input` lines in declaration order (bit names `name[i]`, MSB-first bit strings), or the AIGER symbol table. A witness that does not fit the model is dropped with a note in the engine log. `witness_on_stdout` keeps the raw stdout (not converted). |
| `informal-bmc` (`informal-bmc`, `-10`, `-65`) | values exist, not exported | After SAT it runs `replay()`. That simulates the simplified AIG with the solver's input values for frames 0..=d and its free initial latch values. Everything a trace needs is in `u.sat` at that point. `result()` sets `witness: None`. |
| `informal-kind`, `informal-ic3*`, `informal-kind-ic3`, `informal-wbmc` | no | IC3 finds a CEX as a chain of proof obligations. It doesn't keep the inputs. k-ind's base case is BMC. |
| `informal_fpv::Session` / `Status` | no | `Cex`/`Covered` carry `engine`, `depth`, `ms`. The winner's `witness` is dropped in `check_one`. |
| Name mapping | yes, at bit level | The front end (svlang; slang as a cross-check) writes Btor2 `input`/`state` lines with hierarchical symbols, e.g. `counter.q`, `counter.en`, `counter.rst_n`. `informal_aig::btor::from_btor2` stores them in `Aig::names` (var to `counter.q[2]`). `preprocess::simplify` and `scorr::substitute` copy names for every input or latch they keep. So the simplified AIG that BMC solves still names its inputs and latches. |

Gaps in the names:

- **Combinational nets have no symbol** in the engine model. Examples: `done = (q == 9)` and `nxt`. The Btor2 has only unnamed `eq`/`add` nodes. Since P2d the trace gets them from a separate trace model (see "Named combinational nets" below).
- **The clock isn't in the model.** One Btor2 step is one cycle of the `clock` signal. The VCD adds a synthetic `clk`.
- **FPV helper state shows up.** Examples: `informal_step`, `informal_past_N`, `counter._sva_tick`. The VCD puts it under an `informal` scope that the UI hides by default.
- **Simplify can drop inputs and latches.** Dropped means unused, constant, or merged by scorr. Their witness bits are missing. That's harmless for the property: missing inputs are don't-cares and get 0. The full model is re-simulated (below), so registers still get correct values.

Depth convention: `Unsafe { depth: d }` means the bad output is true in frame `d`, counting from 0. The trace has `d + 1` cycles. On the counter, `a_never9` is `cex depth 10` (q = 9 in frame 10, 11 cycles), found by `informal-bmc-10`. `c_done` is `covered depth 10` by `informal-bmc`.

## Proposed path

1. **informal-bmc fills `witness`.** This is export plumbing only; no algorithm changes. After `replay()` succeeds, the same solver model is written out as text, by name:

   ```
   informalw 1
   depth 10
   init counter.q[0]=0 counter.q[1]=0 ...      # free initial latch values that have names
   @0 counter.en=1 counter.rst_n=0 ...         # named inputs, one line per frame
   @1 ...
   ```

   Only named inputs and latches appear. Unnamed ones only come from AIGER models with no symbol table, which FPV never produces.

2. **informal-fpv `trace.rs`** turns a witness into a VCD:
   - Load the property's own `prop_<i>.btor2` with `informal_aig::load`, unsimplified, so every input and register is present.
   - Simulate frames 0..=d. Named witness bits set the inputs and free inits; missing bits are 0.
   - Check that the bad output is true at frame d. If it isn't, write no trace and log why. We never show a trace that doesn't replay.
   - Group the `name[i]` bits into words and split dotted names into `$scope`s.
   - Add `clk` (high in the first half of each cycle) and the property lane `informal.<prop>`: the bad/cover bit per cycle.
   - Write the result to `<workdir>/<prop>.vcd` (e.g. `informal_work/a_never9.vcd`), timescale 1 ns, one cycle = 10 ns.
3. **The winner has no witness, or it does not replay.** IC3 or k-ind can win, or an external engine whose output informal cannot convert (ABC's witness is for its own network after `undc; zero; fold`, which may add inputs or latches). A converted witness that does not reach the property on the replay model is logged and dropped too. informal-fpv then runs `informal_bmc::Bmc { max_depth: Some(d), step: 1 }` on the same model just to get the trace. It takes milliseconds at the shallow depths FPV sees, and JasperGold also builds traces in a separate step. It's limited to the remaining time budget. If the depth is unknown, it searches up to 64 frames, then gives up.
4. **The session exposes the path.** `Property` gains `trace: Option<String>`, the VCD path. It goes into the snapshot JSON (`props[i].trace`) and is cleared when the property is re-proven. Status words don't change.
5. **The app reads the file.** A Tauri command `read_trace(path)` returns the text only if the path canonicalises inside the session workdir. `backend.ts readTrace(prop)` calls it. The browser mock serves `ui/tests/fixtures/a_never9.vcd`.

## As built

- **Names.** Witness bits carry RTL names (`counter.q[2]`). The VCD has `counter.clk` (synthetic), `counter.rst_n`, `counter.en` and `counter.q[3:0]`, plus `informal.informal_step`, `informal.informal_past_N` and the property lane `informal.<prop>`. AIG-style names (`i3`/`s7`) appear only for Btor2 lines without a symbol. FPV models don't have any, so nothing falls back today.
- **Before reset.** Registers show their free value in cycle 0, the reset cycle. On the real counter, `q` is `1111` in cycle 0, then 0 from cycle 1. That is the model's semantics: reset is asserted in cycle 0 only.
- **Which engine.** On the counter, `informal-bmc`, `informal-bmc-10` and `informal-bmc-65` give the witness directly. With rIC3 winning, its `--cex` witness gives the trace (no re-run; `trace.rs` `counter_traces_from_ric3_witness_without_a_rerun` and `flow.rs` check `Session::trace_reruns` stays 0). With informal's IC3 or k-induction winning, the BMC re-run produces it.
- **Failures.** If the trace can't be built (BMC can't reproduce the result in time, or the replay misses the bad state), the console gets `<prop>: no trace: <why>`. The property keeps its verdict and has no `trace`.

- **Named combinational nets.** With `informal_svlang::Request::trace_outputs`, the Btor2 writer appends a section after the comment line `; informal-trace-outputs`: one `output <node> <path>` line per net or variable driven by a continuous assign, a port connection or a combinational block, sorted by path (e.g. `counter.done`, `counter.nxt`, `counter.wrap`). Nets nothing reads are lowered there too when the lowering can; others are skipped. The section only appends, so the text before the marker is exactly the model without the flag. The FPV session always asks for it, writes the part before the marker to `prop_N.btor2` for the engines (byte-identical to before; checked on the counter's jg and vcf scripts), and the whole text to `prop_N.trace.btor2`, which only the trace replay reads. `from_btor2` keeps named outputs as `Aig::outputs` (engines and preprocessing ignore them), and the VCD writer samples them like registers. The emission is opt-in this way so `output` lines can never change what external engines (rIC3, ABC) see. The Yosys fallback writes no trace model; its traces stay without combinational nets.

## Later
- **Witnesses from rIC3 and ABC.** Mapping them back needs their AIGER witness on our AIGER export, plus the AIGER symbol table. Step 3 covers it for now.
- **Certificate check.** Step 2's re-simulation on the unsimplified model is already the independent witness check the informal-engine docs plan for phase 2 (BtorSim's role).
