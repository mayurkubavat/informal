# InFormal Hands-On Labs (`examples/`)

Self-contained, gate-tested formal verification labs designed to take you from your first proof to Formal Apps, convergence engineering, independent certification, and AI-ready signoff specs.

Every lab uses `[file dirname [info script]]` so it runs cleanly from any working directory, and every lab is executed automatically by [`scripts/test_examples.py`](../scripts/test_examples.py) inside `scripts/check.sh`.

---

## Curriculum Overview

| Track | Lab | Level | Focus | Run command |
| --- | --- | --- | --- | --- |
| **01** | [Quickstart Counter](01-quickstart/counter/README.md) | Learner (5 min) | First proof, CEX, cover, unreachable cover; JG vs. VCF syntax | `informal shell examples/01-quickstart/counter/run.jg.tcl` |
| **02** | [FPV Fundamentals — FIFO](02-fpv-fundamentals/fifo/README.md) | Intermediate (15 min) | Temporal SVA, hierarchical XMR, vacuity, `check_assumptions -dead_end`, `why_cex`, `task` isolation | `informal shell examples/02-fpv-fundamentals/fifo/run.jg.tcl` |
| **03** | [Convergence — Deep Watchdog & Multiplier](03-convergence/deep_counter/README.md) | Expert (20 min) | `get_fv_complexity`, `blackbox_assistant`, `stopat`, `abstract -counter`, `assume -from_assert`, proof cache | `informal shell examples/03-convergence/deep_counter/run.jg.tcl` |
| **04** | [Formal Apps Suite](04-formal-apps/README.md) | Intermediate–Expert | 7 mini-labs: `check_cov`, `check_conn`, `check_xprop`, `check_spv`, `check_sec`, `check_csr`, `check_protocol` | `informal shell examples/04-formal-apps/<app>/run.jg.tcl` |
| **05** | [CI, Certificates & Signoff Spec](05-integration-and-signoff/signoff-spec/README.md) | Production / CI | `fvp.spec.json` (`informal.spec/1`), `Makefile` gate, `shell --check`, `--certificate` verification | `make -C examples/05-integration-and-signoff/signoff-spec fpv-run` |

---

## Running All Labs at Once

From the repository root (after `cargo build --release -p informal-cli`):

```sh
python3 scripts/test_examples.py --bin target/release/informal
```

To audit every Tcl script without running solvers:

```sh
python3 scripts/test_examples.py --bin target/release/informal --check-only
```

---

## Next Steps

- Full User Guide: [docs/guide/getting-started.md](../docs/guide/getting-started.md)
- Command Reference Manual: [docs/reference/tcl-commands.md](../docs/reference/tcl-commands.md)
- Interactive help: run `informal shell --flavor jg` and type `help`
