# Lab 03 — Convergence & Abstractions

**Level:** Expert (20 minutes) · **Directory:** `examples/03-convergence/deep_counter/`  
**Concepts:** `get_fv_complexity`, `blackbox_assistant -apply`, `abstract -counter`, `assume -from_assert`, `stopat`, `task`, and `set_prove_cache`.

## Why This Design Is Hard Without Abstractions

`examples/03-convergence/deep_counter/deep_counter.sv` combines two classic formal bottlenecks:

1. A **24-bit watchdog timer** requiring `1,000,000` clock cycles to reach `wdog_expired == 1` (impossible by raw BMC unrolling).
2. A **16×16-bit multiplier (`u_mul`)** that inflates SAT/AIG gate count when bit-blasted.

## Run It

```sh
informal shell examples/03-convergence/deep_counter/run.jg.tcl
```

## Convergence Pipeline Executed

1. `get_fv_complexity` reports wide-arithmetic hotspots (`wide_arith_ops >= 1`).
2. `blackbox_assistant -apply` boxes `u_mul` so `c_prod_lsb` solves instantaneously.
3. `prove -property a_wdog_bound` proves the counter bound and `assume -from_assert a_wdog_bound` promotes it safely as a helper assumption.
4. `abstract -counter wdog_cnt -threshold 1000000` applies sound counter jump abstraction so `c_wdog_expired` hits `covered` in milliseconds instead of 1,000,000 cycles.
5. Task `FreeCnt` demonstrates isolated `stopat wdog_cnt` without altering `<embedded>`.

## Try It Yourself

1. Comment out `abstract -counter wdog_cnt -threshold 1000000` with `set_prove_time_limit 5s` and watch `c_wdog_expired` become `undetermined`.
2. Re-run `run.jg.tcl` with the same `--workdir` to see SHA-256 proof cache hits (`set_prove_cache on`).
