# Decisions, open issues and ideas to explore

Status as of 2026-11-14. Each decision lists what we chose, the alternatives,
the reason, and the issues still open. Reasons marked *(inferred)* were
reconstructed after the fact. This is a working log from before the project
was public: dates are when a decision was taken, and some entries describe
the single-developer setup of that time.

---

## D1. Wrap existing engines first, then build our own

- **Chosen:** a Rust portfolio (`informal-engine`) over external checkers (rIC3,
  ABC, Pono, BtorMC) as child processes, then in-house engines behind the same
  `Engine` trait. Phase 0 gate: wrapped engines reproduce published HWMCC'24
  counts.
- **Alternatives:** write everything from scratch; link engines as libraries.
- **Why:** the gate validates adapters and verdict parsing before in-house
  engines are compared to them, and rIC3 stays as a soundness oracle.
- **Open issues**
  - Verdicts come from stdout patterns because the tools exit 0 whatever the
    result. Fragile across tool versions.
  - Pono and BtorMC entries are unchecked guesses; AVR, CAR and Avy are not
    added yet.
  - Soundness rests on the engine until certificate checking exists (D8).
- **Ideas:** add AVR (word-level abstraction + IC3), CAR (complementary
  approximate reachability), Avy (interpolation + IC3); a small
  differential-testing job that cross-checks every new engine against rIC3 on
  random small circuits (fuzzing the checkers, not only benchmarks).

## D2. Rust core, C/C++ libraries through FFI

- **Chosen:** Rust workspace; slang, Bitwuzla, CaDiCaL, Tcl 8.6 via FFI shims.
  **Decided 2026-11-09: no ports.** CaDiCaL, Bitwuzla and slang stay C/C++
  behind FFI for good; the only solver we own in Rust is IC3 (with its small
  `Mini` CDCL). Library and dependency upgrades wait until the portfolio is
  level with rIC3.
- **Alternatives:** C++ throughout; pure Rust from day one.
- **Why:** safe concurrency for multi-threaded IC3 with lemma sharing, while
  reusing mature solvers and parser now.
- **Open issues**
  - Two build worlds (cargo plus cmake/meson); `build.rs` links whatever it
    finds, so a missing library silently drops engines or the front end.
  - Porting was considered and rejected (2026-11-09): a pure-Rust CaDiCaL,
    Bitwuzla or SystemVerilog front end would cost years of tuning for no
    verdict gained; effort goes into IC3, preprocessing and certification.
- **Ideas:** make "library missing" a loud warning in `informal info`/CI gate;
  list which engines a given build contains.

## D3. Internal tool, so GPL engines are fine; Verific optional

*Superseded by D16 (2026-11-14): InFormal is open source under Apache-2.0.*

- **Why:** never distributed, so GPL obligations do not bite.
- **Open issues:** if the tool is ever shared outside the company, rIC3 and
  other GPL pieces must be isolated (process boundary only) or replaced. The
  child-process design already helps. Verific is not integrated yet, so
  commercial-grade SystemVerilog coverage rests on slang.

## D4. Pin third-party tools; local patches only, never upstream

- **Chosen:** exact commits in `docs/third-party.md`; patches live on a local
  `informal` branch and are dropped when upstream fixes them.
- **Why:** reproducible builds without maintaining forks.
- **Open issues**
  - Build steps were reconstructed, not logged; the first rebuild may differ.
  - Pins move by hand; no automated "does a newer slang/Bitwuzla change
    verdicts" job (the update rule is manual: check.sh + HWMCC sample).
  - No local patches today, but Bitwuzla carries its own CaDiCaL patches, so
    "no patch" is true only for our checkouts.

## D5. Front end: slang first, Yosys as fallback

*Superseded by D14 (2026-11-14): svlang is the front end, Yosys is gone.*

- **Chosen:** slang elaborates and lowers its AST to Btor2; Yosys is the
  fallback for what lowering does not cover yet.
- **Alternatives:** Yosys only; Verific; Surelog/UHDM; sv2v + Yosys.
- **Why:** slang gives real diagnostics with file/line/column and top-module
  detection, and name checks stop typos becoming vacuous implicit nets.
- **Open issues**
  - Lowering lacks memories, loops, functions, interfaces (falls back to
    Yosys).
  - Counterexample depth is one cycle shorter on the slang path than Yosys
    (Yosys registers clocked assertions) — needs one agreed convention.
  - Signal lookup covers the top module's scope, not sub-instances.
  - VHDL unsupported. Multi-clock ratios (`-factor`, `-phase`), `-both_edges`
    unsupported.
  - Elaboration and model building cannot be interrupted by Stop.
- **Ideas:** Verific adapter behind the same lowering interface; a
  formal equivalence check between the slang and Yosys Btor2 outputs, run in
  tests, to retire the depth discrepancy.

## D6. Btor2 as the IR, AIG as the bit-level core

- **Chosen:** lossless Btor2 reader/writer; bit-blast to an AIG with
  structural hashing; AIGER direct.
- **Alternatives:** an MLIR/CIRCT-style IR; keep everything word-level; use
  only AIGER.
- **Why:** HWMCC models are Btor2/AIGER and bit-level IC3 wins there; word
  level handles arrays.
- **Open issues**
  - Arrays and liveness (`fair`/`justice`) are unsupported at bit level, so
    those runs report unknown. Array witnesses are not supported either.
  - Word-level engines cover only BMC (`informal-wbmc`), no unbounded proof.
  - rIC3 also lacks liveness, so we have no oracle for it.
- **Ideas:** liveness-to-safety (Biere/Artho/Schuppan), k-liveness
  (Claessen/Sörensson), and ranking-function synthesis; word-level IC3 with
  implicit predicate abstraction (IC3IA, as in Pono); array abstraction
  (e.g. lazy array lemmas or Ackermann-style reduction).

## D7. Engine portfolio and its pieces

- **Chosen:** `informal-kind-ic3`, `informal-ic3-mt`, `informal-kind`, `informal-bmc`,
  `informal-bmc-10`, `informal-bmc-65`; in-house small CDCL solver (`Mini`) for IC3 and
  CaDiCaL for unrolling; `scorr` latch correspondence; variable-eliminating
  CNF.
- **Why:** IC3 proves, stepped BMC finds deep bugs, Houdini turns IC3 lemmas
  into k-induction strengthening. 204 of 319 HWMCC'24 bit-level models solved
  with no wrong verdicts across separate per-engine runs, level with rIC3's
  portfolio; as one in-process portfolio run (300 s, 14 GB) it was 184 on
  2026-11-09, 191 after memory shedding (af4e70e).
- **Open issues**
  - We only match rIC3's portfolio, not beat it. 115 models still unsolved.
  - IC3 lacks EXCTG and DynAMic; CTP is off by default and on in one worker.
  - Fixed portfolio: every model gets the same engines regardless of
    structure.
  - Soundness bugs have appeared (unchecked lemma imports gave wrong "safe"
    on a03/a11; a `scorr` equality proof needed a fix). Those were caught by
    comparison, which suggests more may be hiding.
  - Engine memory: 16 rIC3 workers reached 18 GB; a CNF stage once needed
    10.9 GB. In one process the engines share one memory budget: over it,
    the portfolio sheds engines one at a time, deep BMC first (af4e70e), and
    every engine shares one prepared model and CNF template (133a212); each
    engine still loads its own SAT solvers (Problem07 still peaks at ~10 GB).
- **Ideas:** see "Math and algorithm ideas" below.

## D8. Soundness and trust

- **Chosen:** replay every BMC counterexample by simulation; conflicting
  engines give "unknown (engine conflict)"; imported IC3 lemmas are
  re-checked. **Every IC3 SAFE is certified (2026-11-09, fc2a7e9):** the
  final invariant is carried back through simplification and `scorr` (latch
  maps, plus the constant and merged latches they assumed) and checked with
  three fresh SAT queries (init, consecution, safety) on a plain Tseitin
  encoding of the model as loaded, in the cone of the bads, constraints and
  invariant latches (`informal-bmc/src/certify.rs`). A failure withdraws the
  SAFE (unknown, with a loud message).
  - Evidence: re-introducing the old unchecked lemma import made
    `informal-ic3-mt` say SAFE on x-epic a03/a11 (both unsafe); with the
    gate both were withdrawn ("consecution fails"). Merging scorr's
    simulation candidates without proof gave 4+ wrong SAFE on sample60;
    with the gate 36 SAFE were withdrawn and 0 wrong remained.
  - Cost: 4.1 s of checking in 31 s of SAFE runs on sample60 at 30 s (13%,
    mostly sub-second checks on fast solves; at most 0.85 s per model).
  - k-induction SAFE verdicts are certified too: the strengthening (Houdini
    cubes plus the preprocessing facts) is checked inductive, then the base
    (no bad state in frames 0..k-1 from init) and the k-step induction with
    lazily added simple-path constraints over the representative latches.
    sample60 at 30 s: 30 SAFE from `informal-kind`/`informal-kind-ic3`, all
    certified, 8.2 s of checking in 90 s of SAFE runs (9%). With unproved
    scorr merges, `informal-kind` gave 6+ wrong SAFE; the gate withdrew 37
    and left 0 wrong.
- **Open issues:** still trusted: the AIGER/Btor2 reader and bit-blaster,
  `certify.rs`, and
  CaDiCaL's UNSAT answers. Certifaiger (safe) and BtorSim (witnesses) as
  outside checkers are still planned.
- **Ideas:** proof logging (DRAT/LRAT) from CaDiCaL for the three UNSAT
  answers; a verified checker.

## D9. Tcl shell compatible with JasperGold and VC Formal

- **Chosen:** real embedded Tcl 8.6, vendor commands implemented by informal.
- **Alternatives:** a new scripting language; Python API; Tcl look-alike.
- **Why:** existing scripts run unchanged and habits carry over.
- **Open issues**
  - Only a subset of the vendor command set and options is supported;
    unknown options are errors, which is correct but surprising.
  - SVA support is a subset: no `##[m:n]`, repetition, `throughout`,
    `within`, `intersect`, `first_match`, `until`, `s_eventually`, `not`,
    `strong`/`weak`. Liveness operators are missing entirely.
  - The shell thread runs one request at a time, so console commands typed
    during a prove wait; Stop right before a prove starts can be lost.
- **Ideas:** a full SVA-to-automaton compiler (sequence NFA/DFA, then
  assertion monitors), with liveness handled through D6 techniques;
  cross-checking SVA semantics against the IEEE 1800 formal semantics or an
  existing reference implementation.

## D10. Desktop UI: Tauri 2 + React, quiet EDA-style workbench

- **Chosen:** tree / table / console layout, debug in its own tab, colour
  reserved for data; chat-style and colourful layouts rejected.
- **Alternatives:** Electron; native Qt; web app.
- **Why:** small footprint, shared Rust backend, familiar to Jasper/VCF users.
- **Open issues**
  - Tauri on Linux depends on webkit2gtk, whose behaviour differs from
    macOS WKWebView and Windows WebView2; cross-platform parity is unproven.
  - Waveform view is VCD-based; Surfer hand-off is optional and not
    installed here.
  - Visual regression tests run against a browser mock, not the real
    Tauri window.
- **Ideas:** FST support for large traces; cross-probing between property,
  source and waveform everywhere; incremental/streaming trace loading.

## D11. AI is advisory only

- **Chosen:** local Ollama gemma3:12b; Ask mode streams text; the first Tcl
  block is placed on the prompt; Enter runs it. No tools, files or shell.
- **Open issues**
  - 12B local model quality is modest for formal questions; there is no
    evaluation of how often suggested commands are right.
  - The native Messages-API adapter is still a TODO (`config/llm.toml`).
- **Ideas:** use AI where it is checkable: engine/strategy selection
  (decision tree or learned model over the structural features already in
  the run database), property and invariant suggestion where the proof
  engine verifies the result, counterexample explanation.

## D12. Process: local repo, HWMCC'24 benchmark, check.sh as gate

- **Update 2026-11-14:** the repository is public at
  `github.com/mayurkubavat/informal` (D16), which retires the backup
  concern below.
- **Open issues:** no CI; benchmark runs compete with other work on the
  same machine.
- **Ideas:** a nightly bare-repo backup and a scheduled regression run that
  records PAR-2 and verdict disagreements over time.

## D13. The front end fails closed

- **Chosen (2026-11-10):** the slang lowering either models a construct as
  IEEE 1800 defines it, or says it cannot. Three outcomes: lowered;
  *unsupported* (not handled yet, Yosys may: fall back with a note);
  *rejected* (compile errors, and constructs no front end here models:
  the property is an error, with no fallback). Reset and clock names are
  checked against the elaborated design.
- **Why:** an external review (2026-11-10) reproduced wrong verdicts from
  guesses: a misspelt reset became a free wire through the Yosys fallback,
  `$isunknown` read as 0, `==?` as `==`, a second always_ff block on
  another slice replaced the first. HWMCC runs cannot see these: the
  models there never pass through the front end.
- **Gate:** `tests/sv` (known answers, run by `cargo test` and so by
  `scripts/check.sh`). Every lowering fix adds the design that showed it.
- **Open issues:** UNSAFE verdicts are not replayed on the original model
  yet; four-state values; multi-clock designs; black boxes.
- **Update 2026-11-14 (D14):** with svlang there is no fallback at all:
  *unsupported* is an error too, naming the construct.

## D14. svlang is the front end; Yosys is removed

- **Chosen (2026-11-14):** svlang (github.com/mayurkubavat/svlang, pure
  Rust, pinned by commit like the other third-party code, D4) elaborates
  the design and lowers it to a word-level netlist; `informal-svlang`
  writes that netlist as Btor2. Elaboration (diagnostics, top module,
  Design browser tree, name checks) and lowering both use it. The Yosys
  path is removed. The slang lowering stays for now as a cross-check only
  (`INFORMAL_FRONTEND=slang`, no fallback), to be removed once svlang has
  run on more designs.
- **Split:** svlang owns everything up to the netlist (binding, typing,
  procedural semantics, drivers, loops, functions, memories). InFormal owns
  what is formal policy: one clock and edge per model step, async reset
  acting synchronously, X/Z constant bits and undriven bits as free inputs,
  `(* anyconst *)` as a free value held for the whole trace, `$past`
  histories without initial values, labelled immediate asserts as `bad`
  lines and assumes as constraints, memories up to 4096 words as registers
  (larger ones as Btor2 arrays).
- **Why:** one front end with real diagnostics and no C++ build; the slang
  lowering lacked memories, loops and functions, so real designs fell back
  to Yosys, which reads SystemVerilog loosely. svlang handles them: the npu
  RV32I core with its RVFI checks (memories, loops, `(* anyconst *)`)
  lowers through svlang where the slang lowering could not, and gives the
  same verdicts and counterexample depths as the Yosys model on the correct
  core and on three forwarding-network mutants.
- **Gate:** `tests/sv` (20 known answers) and `tests/fpv` (counter, FIFO)
  through svlang in `cargo test`; the FIFO also through slang when built in.
  Gaps found in svlang are fixed in svlang, with a test there.
- **Open issues:** concurrent properties written in the RTL are checked
  when they have a fixed length (booleans, `##N`, `[*N]`, `|->`, `|=>`);
  ranges, repetition ranges, `until` and liveness are not compiled yet (an
  assertion or cover is then an error on that property, an assumption on
  the design); both edges of one clock are modelled with two steps per
  cycle; latches are transparent within a step (loops through open
  latches are rejected); several clocks are rejected;
  `$anyconst` / `$anyseq` as functions are unsupported (the attributes
  work); lowering speed on large designs is not measured yet.

## D15. In-house engines can run as child processes (opt-in)

(D14 is the svlang front end, on its branch.)

- **Chosen (2026-11-14):** `--isolate` (or `INFORMAL_ISOLATE`) runs each
  in-house engine as `informal engine <name> <model>`, a child of the same
  binary that writes JSON lines: progress, then one result with the verdict,
  depth and witness (`informal-engines-ext/src/isolated.rs`). Certificates
  are checked inside the child, as in-process. `informal-ic3-mt` stays one
  process, so its workers keep their shared lemma pool. The default stays
  in-process.
- **Why:** one engine's memory growth or a crash in an FFI library should
  end that engine, not the run. In-process shedding (af4e70e) polls and
  frees memory late; a killed process frees it at once. Each child has the
  budget as its own limit (in-process, and a systemd scope's `MemoryMax`
  where user scopes work), and a monitor keeps the children's total under
  the budget by killing the one with the highest shed rank. The line
  protocol is also the way to plug in an engine written in anything.
- **Cost:** each child loads and prepares the model and builds its own CNF
  template, which in-process engines share (133a212); about 20 ms more per
  check for the processes. Measurements: `docs/engines.md`.
- **Open issues:** not used by the desktop app yet (it has no `informal`
  binary to spawn); lemma or invariant sharing across engines would need
  messages on the protocol; the monitor reads resident memory per process,
  not per cgroup.

---

## Math and algorithm ideas to explore

Ordered roughly by expected payoff for HWMCC-style models.

1. **IC3 refinements:** EXCTG and DynAMic generalisation, which are listed as
   missing in `ic3.rs`; better lemma ordering and push-forward; ternary
   simulation for cube lifting (to compare with the current lifting).
2. **Proof-based abstraction and CEGAR-style refinement** at bit level before
   IC3, to shrink large CPU models.
3. **Interpolation:** McMillan-style interpolation and Avy-style IC3 +
   interpolants as a complementary engine.
4. **Word-level unbounded proof:** IC3IA / IC3 with implicit predicate
   abstraction on Bitwuzla; k-induction over bit-vector terms; SyGuS-style
   invariant synthesis.
5. **Liveness:** liveness-to-safety, k-liveness, and ranking-function
   synthesis, for the properties we report unknown on today.
6. **Preprocessing:** sequential SAT sweeping (beyond `scorr`), retiming,
   phase abstraction, rewriting with ABC-style `dc2`/`&dch`.
7. **Portfolio science:** per-model engine selection from structural
   features, algorithm schedules (time-sliced portfolios), and measuring
   virtual-best gap against our actual portfolio. The run database is the
   training set.
8. **Certificates:** invariants and DRAT/LRAT proofs so a "safe" verdict can
   be checked independently.
9. **Symmetry and compositional methods** for parameterised designs
   (assume-guarantee reasoning, abstraction of counters and FIFOs).
10. **Engines from other fields:** ideas borrowed from SAT, SMT and
    software model checking; add each here with its prediction before it
    is tried.

## Software engineering ideas to explore

- Engine registry that reports capabilities, build flags and library
  versions, so a run record states exactly which engines produced a result.
- Reproducible runs: record tool commit hashes, CPU model and memory cap
  in each run-database line.
- Replace the stdout-pattern verdict parsing with a structured result file
  for engines we control (rIC3 `--cex`/JSON where available).
- Make elaboration and model building cancellable (isolate in a process).
- A second interpreter or event loop so console commands work during a prove.
- Fuzz and differential testing: random small circuits, all engines must
  agree; mutation testing on the sound-critical parts of IC3 and `scorr`.
- A minimal CI substitute that runs `check.sh` on every local commit (git
  hook) and a nightly benchmark.
- Cross-platform status table for the desktop app (Linux, macOS, Windows).

---

## External literature review (2026-11-09)

Source: an AI-generated literature review of D1-D12 and the ideas above
(the report is not in this repository; the prompt is
`docs/research-prompt-decisions.md`). **Treat it as a
literature-backed hypothesis list, not as measured fact:** its HWMCC'24/'25
numbers, arXiv references (some dated 2026) and projected solve counts
were not checked by us. Every projection below becomes a plan only after our own
before/after run, with the prediction written down first. Section numbers
refer to the report.

### Headline findings

1. **Certify every SAFE verdict now (report P1).** Check IC3's final frame with
   three independent CaDiCaL queries (Init, Consecution, Safety) against the
   *un-preprocessed* AIG, then Certifaiger/BtorSim; optionally LRAT proofs.
   Predicted under 0.5% wall-clock. Re-introducing the old a03/a11 and
   `scorr` bugs on a test branch should make the gate catch both.
2. **Close the gap to rIC3 (report P2-P4).** The report says rIC3 solves
   248/319 in the official 16-thread HWMCC'24 run (274 in '25), against our
   204. Proposed causes and fixes, in order: EXCTG + DynAMic + A-IC3 bandit +
   stall-aware push budgets (predicted 219-226 solved); turn `Mini` into a
   GipSAT-class solver with DAG cone-of-influence restriction, O(1) bucket
   VSIDS and temporary clauses (predicted 3x faster queries); parent-lemma
   CTP prediction. Note the 248 figure is a 16-thread official run, our 204 is
   a 300 s local run: the gap may be smaller than it looks, so re-measure
   rIC3 on the same machine first.
3. **Backup the repo (report P1/D12).** Done: the repository is on GitHub.

### Where the report agrees, disagrees or adds to each decision

| Decision | Report verdict | Our read |
| --- | --- | --- |
| D1 wrap engines | Modify: stop parsing stdout; validate certificate/witness files; add SuperCAR and AVR; fix Pono/BtorMC args. | Agree on file-based verdicts (matches D8). New engines: add after the P1 gate exists. |
| D2 Rust + FFI | Change: make missing C++ libs a hard compile error via Cargo features; do **not** port CaDiCaL, Bitwuzla or slang to Rust; put pure-Rust effort into `Mini`. | Agree. This **narrows** the "move must-have libs to Rust over time" plan: only the IC3 solver is worth owning. Confirmed. |
| D3 GPL ok | Keep; skip Verific unless VHDL needed; says rIC3 and Pono are BSD-3. | **Check:** rIC3's `LICENSE` file is BSD-3-Clause but its `Cargo.toml` says `GPL-3.0`. Conflicting upstream; irrelevant while InFormal only runs it as a separate program (D16). |
| D4 pinning | Modify: reproducible build script or container; automated sample gate on pin update. | Agree; the build steps are still reconstructed, not logged. |
| D5 slang + Yosys | Change: use `yosys-slang` (the fork rIC3 uses) instead of raw Yosys, with `chformal -early`, to remove the 1-cycle depth mismatch and the second parser. | Plausible and cheap to try; verify with a bounded miter between the two paths. |
| D6 Btor2 + AIG | Keep, plus a sidecar `InformalSymbolMap` from AIG literals to Btor2 nodes and slang scopes; enables IC3-INN and sub-instance lookup. | Agree; it also fixes the "top-scope signals only" limit. |
| D7 portfolio | Change: share one `Arc<Aig>` across workers; cap variable-elimination blowup (|pos|,|neg| <= 8, no literal growth); select 3-4 engines per model instead of a fixed set; add SuperCAR/CAR-DT, Pono, AVR; IC3 with internal signals. | Agree with memory and selection fixes (addresses the 18 GB and 10.9 GB spikes). Engine additions are speculative until measured. |
| D8 trust | Change now: the P1 gate moves from phase 2 to phase 1. | Agree; reverses the "deferred" note above. |
| D9 Tcl shell | Change: run the Tcl REPL on its own thread with an async channel and shared cancel flag polled inside slang lowering, CaDiCaL terminate callback and `Mini`; warn rather than error on benign tuning flags. | Agree; fixes two known limitations (typed commands wait, Stop race). The "warn on unknown options" part conflicts with our deliberate strictness: only for flags known to be tuning-only. |
| D10 UI | Modify: FST traces with streamed windows; WebDriver tests on the real WebKitGTK window. | Agree, medium priority. |
| D11 AI | Change: use AI only where the solver checks it: engine selection (`Btor2-Select`), A-IC3 bandit, and CTI-guided invariant synthesis ("CIll") gated by three solver checks. | Agrees with the existing "advisory only" rule and the ideas above; Ask mode stays. |
| D12 process | Change today: backup remote, merge Mac research docs, git hook + nightly run. | Agree on backup and hook; see headline 3. |

### New ideas from the report to add to the lists above

- Liveness: L2S + BMC for lasso bugs, k-liveness with stabilizing constraints
  and IC3 frame reuse; checked against another tool (report P8).
- Complete SVA through an automaton/Büchi lowering on the slang AST,
  validated against SymbiYosys (report P9).
- `Btor2-Select`: a learned engine selector trained on our own run database
  with cross-validation (report P7).
- IC3 with internal signals (comparators, mux selects) to compress
  miter/datapath invariants (report P6).
- Interval abstract interpretation as invariant seeding and SyGuS/ICE
  word-level lemma synthesis (report section 7).

### Report's ranking, with our status

| # | Item | Effort (report) | Status |
| --- | --- | --- | --- |
| 1 | Backup + 3-query CaDiCaL gate + Certifaiger/BtorSim | 3 days | proposed |
| 2 | EXCTG, DynAMic, A-IC3, stall-aware push | 5 days | proposed |
| 3 | GipSAT-class `Mini` | 8 days | proposed |
| 4 | Parent-lemma CTP prediction | 5 days | proposed |
| 5 | SuperCAR/CAR-DT, Pono, AVR, file-validated verdicts | 6 days | proposed |
| 6 | IC3-INN + `InformalSymbolMap` | 8 days | proposed |
| 7 | `Btor2-Select`, shared `Arc<Aig>`, bounded CNF elimination | 6 days | proposed |
| 8 | Liveness subsystem | 7 days | proposed |
| 9 | Full SVA + `yosys-slang` + async Tcl | 12 days | proposed |
| 10 | Solver-verified CTI-guided LLM invariants | 8 days | proposed |

Effort figures and solve-count gains are the report's, not ours. Suggested
first step: items 1 and 2 together, since the gate makes the IC3 changes safe
to try.

## D16. Open source under Apache-2.0, contributions under a CLA

- **Chosen (2026-11-14):** the repository is public under the Apache License
  2.0 (`LICENSE`, `NOTICE`), copyright Mayur Kubavat. Outside contributions
  need the Contributor License Agreement in `CLA.md`: contributors keep
  their copyright and grant a license that includes the right to relicense.
- **Alternatives:** MIT (simpler, no patent grant); GPL-3.0 (blocks closed
  forks, but puts off companies that would adopt it); a DCO sign-off instead
  of a CLA (lighter, but no relicensing right).
- **Why:** Apache-2.0 matches svlang, lets companies adopt the tool, and its
  patent grant matters in EDA. The CLA keeps the option of other licenses
  (for example a commercial edition) open.
- **Licenses of what InFormal uses:** nothing third-party is vendored or
  patched in this repository. Statically linked: CaDiCaL, Bitwuzla, slang
  (MIT; Bitwuzla's symfpu is GPL-3.0 or BSD-3-Clause at our choice). Dynamically
  linked: Tcl (BSD-style), GMP and MPFR (LGPL-3.0). Separate programs: rIC3,
  ABC. All allow an Apache-2.0 InFormal. Full table: `docs/third-party.md`.
- **Open issues**
  - rIC3's `LICENSE` file says BSD-3-Clause but its `Cargo.toml` says
    GPL-3.0. Harmless while InFormal only runs it as a process; settle it
    before shipping rIC3 binaries with InFormal.
  - Binary releases need a third-party notices file (the MIT notices of the
    linked libraries, the LGPL notice for GMP/MPFR, and the Rust and npm
    dependency licenses, e.g. from `cargo about`).
  - Signing the CLA is manual until a CLA bot is set up on GitHub.
