# Hands-On Labs Curriculum

All runnable RTL, Tcl scripts, specs, and Makefiles live under `examples/` at the repository root and are executed automatically on every gate run by `scripts/test_examples.py`.

| Track | Lab Guide | Source Directory | Level | Key Commands & Concepts |
| --- | --- | --- | --- | --- |
| **01** | [Quickstart Counter](01-quickstart.md) | `examples/01-quickstart/counter/` | Learner (5 min) | `analyze`, `elaborate`, `assert`/`cover`/`assume`, `prove`, `why_cex`, JG vs. VCF |
| **02** | [FPV Fundamentals (FIFO)](02-fpv-fundamentals.md) | `examples/02-fpv-fundamentals/fifo/` | Intermediate (15 min) | Temporal SVA, XMR, vacuity, `check_assumptions -dead_end`, `-with_proof_core`, `task` |
| **03** | [Convergence & Abstractions](03-convergence.md) | `examples/03-convergence/deep_counter/` | Expert (20 min) | `get_fv_complexity`, `blackbox_assistant`, `abstract -counter`, `assume -from_assert`, `stopat`, proof cache |
| **04** | [Formal Apps Suite](04-formal-apps.md) | `examples/04-formal-apps/` | Intermediate–Expert | All 7 apps: `check_cov`, `check_conn`, `check_xprop`, `check_spv`, `check_sec`, `check_csr`, `check_protocol` |
| **05** | [CI, Certificates & Signoff](05-integration-and-signoff.md) | `examples/05-integration-and-signoff/signoff-spec/` | Production / CI | `fvp.spec.json` (`informal.spec/1`), `Makefile` gate, `shell --check`, `--certificate`, `spec_audit` |

## Run Every Lab in One Command

From the repository root:

```sh
python3 scripts/test_examples.py --bin target/release/informal
```

To audit every lab's Tcl script without starting solvers:

```sh
python3 scripts/test_examples.py --bin target/release/informal --check-only
```
