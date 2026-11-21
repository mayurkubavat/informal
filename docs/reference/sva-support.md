# SystemVerilog Assertions (SVA) & Lowering Reference

InFormal compiles SystemVerilog designs and SVA properties (both RTL-embedded and Tcl-supplied) through the same pure-Rust `svlang` front end into word-level Btor2 (`informal lower` / `informal shell`).

> **Core soundness rule (decision D13):** Constructs `svlang` cannot model faithfully are **rejected with an explicit diagnostic** naming the construct — never silently guessed or approximated.

---

## Supported SVA Constructs (IEEE 1800-2023)

| Category | Supported syntax | Notes |
| --- | --- | --- |
| **Clocking & Reset** | `@(posedge clk)`, `@(negedge clk)`, `disable iff (expr)` | Single primary clock or declared multi-clock ratios (`clock -factor / -phase`) |
| **Abort Operators** | `accept_on`, `reject_on`, `sync_accept_on`, `sync_reject_on` | Synchronous and asynchronous property aborts |
| **Cycle Delays** | `a ##N b`, leading `##N`, bounded `##[M:N]`, unbounded `##[M:$]` | Compiled to finite automata monitors |
| **Repetitions** | Consecutive `[*N]`, `[*M:N]`, `[*M:$]`; goto `[->N]`, `[->M:N]`; non-consecutive `[=N]`, `[=M:N]` | Supported inside sequences and implications |
| **Sequence Composition** | `and`, `or`, `intersect`, `within`, `throughout`, `first_match` | Full sequence combinators |
| **Implications & Followed-By** | `\|->`, `\|=>`, `#-#`, `#=#` | Precondition covers automatically synthesized for `\|->` / `\|=>` unless `-no_preconditions` |
| **Property Operators** | `and`, `or`, `not`, `implies`, `iff`, `if (b) p1 [else p2]`, `case (...) ... endcase` | Includes IEEE 1800-2023 §16.12.17 SVA `case` |
| **Temporal & Liveness** | `until`, `until_with`, `s_until`, `s_until_with`, `s_eventually`, `eventually`, `nexttime [N]`, `s_nexttime`, bounded & compound `always` (`always [M:N]`, `start \|=> always p`) | Strong/liveness properties lower to Btor2 `justice` and are solved by in-house `informal-klive` / L2S |
| **Local Variables** | Sequence local variables (`acc += din`, `acc++`, unbounded `##[1:$]` / `until_with` tag scoreboards) | IEEE 1800-2023 §16.10 bounded & scoreboard forms |
| **Sampled-Value Functions** | `$past(x[, n])`, `$rose(x)`, `$fell(x)`, `$stable(x)`, `$changed(x)` | Plus pure RTL expressions, module/package functions, and XMR (`top.u_sub.sig`) |
| **Immediate Assertions & Covers** | Procedural `assert (...)` / `cover (...)` inside `always`, loops, and tasks | Unrolled across static loops and multiple instances with unique hierarchical names |
| **Fairness Assumptions** | `assume property (s_eventually cond)` | Enforced as fairness constraints for liveness proofs |

---

## Liveness Semantics

- `s_eventually b` and the strong forms `s_until` / `s_until_with` emit Btor2 `justice` targets.
- In-house engines (`informal-klive` and liveness-to-safety `informal_aig::l2s`) check them natively. External bit-level engines are skipped on liveness models.
- A liveness `cex` is a lasso trace (stem + repeating loop); the VCD header comment identifies the loop start frame.

---

## Explicit Lowering Rejections & Opt-In Relaxations

By default, InFormal rejects ambiguous or unmodeled constructs during `elaborate` / `informal lower`:

| Construct | Default behavior | Opt-in escape hatch |
| --- | --- | --- |
| Time-0 checks in `initial` blocks | Rejected (`NOT MODELLED`) | Tcl: `set_lowering skip_initial_checks on` · CLI: `informal lower --skip-initial-checks` |
| `$isunknown(expr)` (4-state check in 2-state FPV) | Rejected | Tcl: `set_lowering isunknown_is_false on` · CLI: `informal lower --isunknown-is-false` |
| Multiple drivers on the same register bits | Rejected | Fix RTL (or inspect via `formal_lint`) |
| Undeclared clock domains | Rejected | Declare clocks via `clock` / `create_clock` |
| VHDL sources (`analyze -vhdl`) | Rejected | Convert to SystemVerilog or black-box with `-bbox_m` |
| Large unpacked memories (`> 4096` words) | Kept as word-level Btor2 `Sort::Array` (solved by `informal-wbmc` / `informal-wkind`; bit-level engines skip) | Auto-box via `elaborate -bbox_a N` or use word-level engines |
| Undeclared signal names in Tcl properties | Rejected immediately at `assert`/`assume`/`cover` | Prevents typos from turning into implicit nets |

---

## Checking Design Lowering Standalone

To inspect the exact Btor2 netlist lowered from SystemVerilog without starting a Tcl session:

```sh
informal lower rtl/counter.sv --top counter -D FORMAL
```

For full details on how `svlang` binds hierarchical names and compiles properties, see [docs/fpv-shell.md](../fpv-shell.md).
