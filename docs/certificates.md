# Certificates

A verdict from `informal check` can be written out so that a program that
shares no code with InFormal confirms it. This is what the Hardware Model
Checking Competition asks of its entrants, in the same formats.

```sh
target/release/informal check model.aig --certificate certs/
python3 -I scripts/certificate_check.py certs/
```

The first command prints one more line, for example
`certificate: model.aig.cert.aig (invariant, for certifaiger)`, and writes
into `certs/`:

| File | When | Format | Checked by |
| --- | --- | --- | --- |
| `<model>.trace` | UNSAFE | AIGER witness (`1`, `b<k>`, initial latch values, one line of input values per step, `.`) | `aigsim` of the AIGER tools: it simulates the trace on the model and must reach the bad output |
| `<model>.cert.aig` | SAFE | witness circuit, binary AIGER | Certifaiger: it writes its conditions as circuits, and a SAT solver must find each one unsatisfiable |
| `<model>.model.aig` | Btor2 models | the model bit-blasted, binary AIGER | the model the two checkers above read |
| `<model>.cert.json` | always | summary: model, verdict, engine, kind of evidence, file and checker, or the reason there is no file | `scripts/certificate_check.py` |

`<model>` is the model's file name with its extension. The second command
runs the checkers on every summary in the directory and prints how many
verdicts were confirmed, how many have nothing to check and why, and which
failed. Its exit status is 1 when a certificate is wrong.

## What is trusted, with and without a certificate

Every SAFE and UNSAFE of the in-house engines is already checked inside
the tool, on the model as loaded and with an encoding of its own
(`informal-bmc/src/certify.rs`, decision D8). That check still shares the
AIGER reader, the checking code and the SAT solver with the engines. With
a certificate confirmed outside, an AIGER model's verdict rests on
Certifaiger or aigsim, the AIGER library they read the model with, and the
SAT solver that Certifaiger's conditions are given to. Nothing of InFormal
is left in that list: a wrong witness circuit or trace is rejected, it
cannot make a wrong verdict pass.

For a Btor2 model the checkers read the bit-blasted model that InFormal
wrote, so InFormal's Btor2 reader and bit-blaster stay trusted, and for a
liveness property the liveness-to-safety step too. The summary's `note`
says so. The gate's BtorSim replay (`scripts/btorsim_replay.py`) covers
counterexamples of Btor2 models without that.

## Witness circuits

A witness circuit is an AIGER circuit with one bad output. Write `P'` for
"its bad output is false". Certifaiger accepts it for a model when:

- **Reset and transition:** on the inputs and latches the two circuits
  share, the witness circuit starts and steps as the model does;
- **Safety:** where `P'` holds, the model's bad outputs are false;
- **Base:** `P'` holds in the witness circuit's initial states;
- **Inductive:** a step from a state where `P'` holds leads to one where
  it holds.

Constraints are assumed in each of them, in the states they talk about.
Together they say that no bad state of the model is reachable.

InFormal's witness circuits hold the part of the model the proof is about
(the cone of the bad outputs, the constraints and the latches the proof
names), copied gate for gate, and the symbol table maps each of its inputs
and latches to the model's literal (`i0 = 2`, `l0 = 14`). What is added
depends on the proof.

**Inductive invariant** (IC3). Nothing but the output: `P'` is "no bad
output, and every clause of the invariant". The invariant includes what
preprocessing assumed: latches found constant and latches merged.

**k-induction.** The circuit remembers the last `k - 1` states: for each,
a copy of the cone's latches and inputs and a bit that says the copy is
filled. `P'` says that the present state is good and that every filled
copy is a good state that meets the constraints and the strengthening
invariant and leads to the next one; and, while not all copies are filled,
that the oldest one is an initial state. The inductive condition is then
both halves of the k-induction proof: with all copies filled it is the
step (`k` good states in a row are followed by a good one), and with fewer
it is the base (a path of fewer than `k` steps from an initial state stays
good). The circuit grows with `k`: `k` copies of the cone.

This covers k-induction without the simple-path condition. The engine
looks for loop-free paths only when it has to; the independent check
counts how often it had to tell two states to differ, and when that count
is not zero the proof has no witness circuit and the summary says so.

**Two-step invariant** (IC3 on a clock-folded model, where one step is two
of the loaded model). The circuit remembers one state and has one more
latch that is true on odd steps. `P'` is: no bad output; on even steps the
invariant; on odd steps, the remembered state satisfied the invariant and
the constraints and leads to the present one. That is how
`certify::check_folded` reads the invariant: it holds on the clock's
initial phase and again two steps later, and neither that state nor the
next one is bad.

## What has no certificate outside yet

| Verdict | Why | Summary says |
| --- | --- | --- |
| SAFE by k-induction that needed the simple-path condition | "The path does not repeat a state" is not a property of the last `k` states alone | `the k-induction proof needs the simple-path condition` |
| SAFE or UNSAFE from an external engine (rIC3, ABC) | their own certificates are not collected | `<engine> gives no certificate` |
| Btor2 models with an initial value given by an expression | AIGER as InFormal writes it has constant initial values only | `initial values given by expressions` |
| Word-level BMC (`informal-wbmc`), models with arrays | no bit-level model | `informal-wbmc gives no certificate` |
| Verdicts with `INFORMAL_NO_CERT` set | the independent check that produces the certificate was skipped | `gives no certificate` |

## From the Tcl shell

`informal shell script.tcl --certificate certs/` writes the same files for
every property the in-house engines settle, named after the property's
model in the work directory, and logs one `certificate:` line per
property. A cover that is reached leaves a trace, an unreachable one a
proof. The models are Btor2, so the bit-level note above applies to all of
them; properties settled during elaboration have no model and no
certificate.

## The checkers

`scripts/certificate_check.py` finds them in `$INFORMAL_TOOLS_DIR`
(`docs/third-party.md` has the commits and build steps): Certifaiger, the
AIGER tools `aigsim`, `aigsplit` and `aigtocnf`, and Kissat, the SAT solver
Certifaiger's own scripts use. Certifaiger writes one circuit with an
output per condition; `aigsplit` makes one circuit of each, `aigtocnf` a
CNF, and Kissat must answer unsatisfiable (exit status 20) for all of them.

```sh
python3 -I scripts/certificate_check.py certs/ --report report.json
python3 -I scripts/certificate_check.py --models tests/models \
    --expect invariant,k-induction,two-step,trace
```

The second form runs every in-house engine on each AIGER model of
`tests/models` and checks what they wrote; `--expect` makes it fail when a
kind of certificate was never confirmed.
