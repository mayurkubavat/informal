# Decisions, open issues and ideas to explore

Status as of 2026-11-09. Each decision lists what we chose, the alternatives,
the reason, and the issues still open. Reasons marked *(inferred)* are not
written down elsewhere in the repo; confirm or correct them.
Sources: README, `docs/`, git log, project notes.

---

## D1. Wrap existing engines first, then build our own

- **Chosen:** a Rust portfolio (`fdv-engine`) over external checkers (rIC3,
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

- **Chosen:** Rust workspace; slang, Bitwuzla, CaDiCaL, Tcl 8.6 via FFI shims;
  move must-have libraries to Rust over time.
- **Alternatives:** C++ throughout; pure Rust from day one.
- **Why:** safe concurrency for multi-threaded IC3 with lemma sharing, while
  reusing mature solvers and parser now.
- **Open issues**
  - Two build worlds (cargo plus cmake/meson); `build.rs` links whatever it
    finds, so a missing library silently drops engines or the front end.
  - Rust-native replacements are not yet scoped: which libraries are
    "must-have" is undecided. A pure-Rust SAT solver (e.g. a CaDiCaL port)
    would remove C++ from the engine path but costs performance and effort.
- **Ideas:** make "library missing" a loud warning in `fdv info`/CI gate;
  list which engines a given build contains.

## D3. Internal tool, so GPL engines are fine; Verific optional

- **Why:** never distributed, so GPL obligations do not bite.
- **Open issues:** if the tool is ever shared outside the company, rIC3 and
  other GPL pieces must be isolated (process boundary only) or replaced. The
  child-process design already helps. Verific is not integrated yet, so
  commercial-grade SystemVerilog coverage rests on slang.

## D4. Pin third-party tools; local patches only, never upstream

- **Chosen:** exact commits in `docs/third-party.md`; patches live on a local
  `fdv` branch and are dropped when upstream fixes them.
- **Why:** reproducible builds without maintaining forks.
- **Open issues**
  - Build steps were reconstructed, not logged; the first rebuild may differ.
  - Pins move by hand; no automated "does a newer slang/Bitwuzla change
    verdicts" job (the update rule is manual: check.sh + HWMCC sample).
  - No local patches today, but Bitwuzla carries its own CaDiCaL patches, so
    "no patch" is true only for our checkouts.

## D5. Front end: slang first, Yosys as fallback

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
  - Word-level engines cover only BMC (`fdv-wbmc`), no unbounded proof.
  - rIC3 also lacks liveness, so we have no oracle for it.
- **Ideas:** liveness-to-safety (Biere/Artho/Schuppan), k-liveness
  (Claessen/Sörensson), and ranking-function synthesis; word-level IC3 with
  implicit predicate abstraction (IC3IA, as in Pono); array abstraction
  (e.g. lazy array lemmas or Ackermann-style reduction).

## D7. Engine portfolio and its pieces

- **Chosen:** `fdv-kind-ic3`, `fdv-ic3-mt`, `fdv-kind`, `fdv-bmc`,
  `fdv-bmc-10`, `fdv-bmc-65`; in-house small CDCL solver (`Mini`) for IC3 and
  CaDiCaL for unrolling; `scorr` latch correspondence; variable-eliminating
  CNF.
- **Why:** IC3 proves, stepped BMC finds deep bugs, Houdini turns IC3 lemmas
  into k-induction strengthening. 204 of 319 HWMCC'24 bit-level models solved
  with no wrong verdicts, level with rIC3's portfolio.
- **Open issues**
  - We only match rIC3's portfolio, not beat it. 115 models still unsolved.
  - IC3 lacks EXCTG and DynAMic; CTP is off by default and on in one worker.
  - Fixed portfolio: every model gets the same engines regardless of
    structure.
  - Soundness bugs have appeared (unchecked lemma imports gave wrong "safe"
    on a03/a11; a `scorr` equality proof needed a fix). Those were caught by
    comparison, which suggests more may be hiding.
  - Engine memory: 16 rIC3 workers reached 18 GB; a CNF stage once needed
    10.9 GB.
- **Ideas:** see "Math and algorithm ideas" below.

## D8. Soundness and trust

- **Chosen:** replay every BMC counterexample by simulation; conflicting
  engines give "unknown (engine conflict)"; imported IC3 lemmas are
  re-checked.
- **Open issues:** invariants are not certified. Certifaiger (for safe
  verdicts) and BtorSim (for witnesses) are planned for phase 2.
- **Ideas:** emit an inductive-invariant certificate from IC3 and check it
  with an independent SAT call (or a verified checker); proof logging
  (DRAT/LRAT) from CaDiCaL for the underlying UNSAT answers.

## D9. Tcl shell compatible with JasperGold and VC Formal

- **Chosen:** real embedded Tcl 8.6, vendor commands implemented by fdv.
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

- **Open issues:** no CI, no remote, so a disk loss loses the history; the
  Mac copy holds research docs that the repo does not; benchmark runs
  compete with other work on the same box.
- **Ideas:** a nightly bare-repo backup and a scheduled regression run that
  records PAR-2 and verdict disagreements over time.

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
10. **Engines from other fields:** the idea file
    `research/docs/cross-field-engine-ideas.md` lives on the Mac copy only;
    fold its predictions and results into this list once copied over.

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
