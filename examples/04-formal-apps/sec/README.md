# Formal App Lab — Sequential & Combinational Equivalence (`check_sec`)

Exercises four equivalence modes in one script:
1. Sequential SEC (`spec.sv` vs. algebraically refactored `imp_ok.sv` → `proven`)
2. Combinational keypoint LEC (`-lec` → `proven`)
3. Retimed pipelined SEC (`-latency 1` against 2-stage `imp_pipe.sv` → `proven`)
4. Corner-case bug detection (`imp_bug.sv` fails when `a == 8'hA5` → `cex` with side-by-side `shared`/`spec`/`imp` VCD + `why_cex`)

```sh
informal shell examples/04-formal-apps/sec/run.jg.tcl
```
