# Lab 03 — Proof Convergence & Abstractions

**Level:** Expert (20 minutes)  
**Concepts:** `get_fv_complexity`, `blackbox_assistant -apply`, `abstract -counter`, `assume -from_assert`, `stopat`, `task`, and `set_prove_cache`.

## Why This Design Is Hard Without Abstractions

`deep_counter.sv` combines two classic formal bottlenecks:
1. A **24-bit watchdog timer** requiring `1,000,000` clock cycles to reach `wdog_expired == 1` (raw BMC cannot unroll a million cycles).
2. A **16×16-bit multiplier (`u_mul`)** that explodes SAT/AIG gate count when bit-blasted.

## Run It

```sh
informal shell examples/03-convergence/deep_counter/run.jg.tcl
```

## Convergence Steps Executed

1. `get_fv_complexity` reports wide-arithmetic hotspots (`wide_arith_ops >= 1`).
2. `blackbox_assistant -apply` automatically boxes `u_mul` so `c_prod_lsb` solves instantaneously.
3. `prove -property a_wdog_bound` proves the counter upper bound and `assume -from_assert` safely promotes it as a helper assumption.
4. `abstract -counter wdog_cnt -threshold 1000000` applies sound counter jump abstraction so `c_wdog_expired` hits `covered` in milliseconds instead of 1,000,000 cycles!
5. Task `FreeCnt` isolates a `stopat wdog_cnt` experiment without touching `<embedded>`.

## Try It Yourself

1. Comment out `abstract -counter wdog_cnt -threshold 1000000` and lower `set_prove_time_limit 5s` — observe `c_wdog_expired` become `undetermined`.
2. Re-run `run.jg.tcl` a second time with the same `--workdir` to observe instant SHA-256 proof cache hits (`set_prove_cache on`).
