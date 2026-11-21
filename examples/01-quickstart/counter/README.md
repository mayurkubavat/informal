# Lab 01 — Quickstart Counter

**Level:** Learner (5 minutes)  
**Concepts:** `analyze`, `elaborate`, `clock`, `reset`, `assert`/`cover`/`assume`, `prove`, `why_cex`, JasperGold vs. VC Formal syntax.

## Run It

```sh
# JasperGold flavor (auto-detected)
informal shell examples/01-quickstart/counter/run.jg.tcl

# VC Formal flavor (auto-detected)
informal shell examples/01-quickstart/counter/run.vcf.tcl
```

## Expected Verdicts

| Property | Kind | Expected Status | Why |
| --- | --- | --- | --- |
| `a_max` | assert | `proven` | `q` stops incrementing at `9` |
| `a_step` | assert | `proven` | Enabled below `9` always increments by `1` |
| `a_hold` | assert | `proven` | Disabled `en` holds `q` stable |
| `a_no_wrap` | assert | `proven` | `wrap` is tied low |
| `a_never9` | assert | `cex` (depth 10) | Intentional failure — `q` reaches `9` after 9 enabled cycles |
| `c_done` | cover | `covered` (depth 10) | Confirms `done` is reachable under `m_en_burst` |
| `c_10` | cover | `unreachable` | Confirms illegal state `q == 10` can never occur |

`run.jg.tcl` also prints `why_cex -property a_never9`, showing the backward causal chain from `q == 9` back through `en` and `rst_n`.

## Try It Yourself

1. **Inspect the waveform:** Run with `INFORMAL_WAVE_VIEWER=surfer` (or `gtkwave`) and evaluate `visualize -violation -property a_never9` in an interactive shell.
2. **Introduce a bug:** Change `else if (en && !done)` in `counter.sv` to `else if (en)` and re-run — watch `a_max` fail with a CEX and `c_10` flip from `unreachable` to `covered`!
3. **Explore interactive help:** Run `informal shell --flavor jg` and try `help prove` and `help why_cex`.
