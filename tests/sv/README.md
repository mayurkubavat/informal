# SystemVerilog known answers

Tiny designs, each with a known answer, run through the whole shell flow
(slang lowering, the injected harness, the in-house engines) by
`crates/informal-fpv/tests/sv_semantics.rs`, so `cargo test` and
`scripts/check.sh` catch a front-end change that gives a wrong verdict.

Each `.sv` file carries its own script and expectations in comments:

- `//% <command>`: a Tcl command, run after `analyze` and `elaborate -top <file stem>`.
- `//= <property> <status>`: the expected status after `prove -all`
  (`proven`, `cex`, `error`, ...).
- `//! <text>`: the script must fail with an error containing `<text>`
  (no `prove` is run).

Add a case whenever a lowering bug is fixed: the design that showed it, with
the answer IEEE 1800 gives.
