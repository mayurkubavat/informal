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
properties moved. To see how a model changed, run the design at both
commits and compare `prop_N.btor2` in the session's working directory.
