# SystemVerilog known answers

Tiny designs, each with a known answer, run through the whole shell flow
(svlang lowering, the injected harness, the in-house engines) by
`crates/informal-fpv/tests/sv_semantics.rs`, so `cargo test` and
`scripts/check.sh` catch a front-end change that gives a wrong verdict.
Each design is its own test, `sv_<file stem>` (the crate's build script
lists the directory), so one design can be run alone:
`cargo test -p informal-fpv --test sv_semantics sv_latches`.

Each `.sv` file carries its own script and expectations in comments:

- `//% <command>`: a Tcl command, run after `analyze` and `elaborate -top <file stem>`.
- `//= <property> <status>`: the expected status after `prove -all`
  (`proven`, `cex`, `error`, ...).
- `//! <text>`: the script must fail with an error containing `<text>`
  (no `prove` is run).

Add a case whenever a lowering bug is fixed: the design that showed it, with
the answer IEEE 1800 gives.

## The lowering record

`lowering.lock` has one line per property that gets a model: design,
property, a digest of the Btor2 model the engines are given, and its size
(lines, states, inputs, checks). The same test compares it on every run.
A change that should not touch the lowering (a refactor, an engine or UI
change) must leave the file alone. When svlang is re-pinned or the lowering
is changed on purpose, rewrite it with

    INFORMAL_BLESS=1 cargo test -p informal-fpv --test sv_semantics

and say in the commit why the models changed: the diff lists exactly which
properties moved.

Whether a changed model still means the same is a separate question, and
`scripts/btor2_equiv.py <old.btor2> <new.btor2>` answers it: it proves that
two models have the same transition system (inputs and registers matched by
name, every next-state function, initial value, constraint and check
compared), or shows that they differ. `scripts/repin.sh` runs it on every
model a new svlang pin changes. `INFORMAL_KEEP_MODELS=<dir>` makes the test
keep a copy of each model for it.

## The same property in RTL form

A property given by command and the same property written in the RTL take
different paths into the model. `rtl_form_gives_the_same_answers` rewrites
every design whose script is a plain clock, a reset and assert/cover
commands (26 of them today): each command property is added to the top
module as `rf_<name>: assert property (...)` under a default clocking and
default disable for the script's clock and reset, and must get the answer
the command form is expected to get. A new design of that shape is covered
without writing anything.
