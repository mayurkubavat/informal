# Experiment 1 & WS1A Benchmark Cases

This directory contains tracked, deterministic, reproducible proof-case generators and RTL for the low-power verification paper experiments:

1. **`gen_exp1_upf.py` — Experiment 1: Symbolic PST SAT Scaling ($N \in \{4, 8, 12, 16, 20, 32, 64, 128\}$)**
   - Generates a parameterized IEEE 1801-2018 (UPF 3.1) power intent with `SS_AON` plus $N$ switchable domains `PD_D0..PD_D{N-1}` (`SS_D0..SS_D{N-1}`), each with $k = 3$ states (`ON`, `RET`, `OFF`), alternating `0.90 V` / `0.75 V` active rails, IEEE 1800 bit-indexed `-logic_expr` guards (`pwr_ctrl[i]`, `sleep_req[i]`, `ret_en[i]`), $N/2$ cross-domain `-illegal` dependency rules (`!pwr_ctrl[i] && pwr_ctrl[i+1]` for even $i$), and a `create_pst` table (`ALL_ON`, `ALL_RET`, `ALL_OFF`, `FREE_ACTIVE`).
   - An explicit Cartesian PST table for $k = 3$ states across $N$ domains requires $3^N$ rows ($4.30 \times 10^7$ at $N=16$, $3.49 \times 10^9$ at $N=20$, $1.18 \times 10^{61}$ at $N=128$), whereas InFormal's symbolic SAT encoding (`PstSat`) grows linearly in $N$ and answers all $N(N-1)$ pairwise `can_be_off_while_on` and `voltage_relation` queries.

2. **`ws1a_soc.sv` — WS1A: `CompiledDesign` Cache Cold vs. Warm Benchmark**
   - Multi-module parameterized SoC pipeline (`ws1a_soc` with `ws1a_stage` instances, handshake/credit counters, and concurrent SVA assertions/covers) exercised across `elaborate`, `re-elaborate`, `prove`, `check_xprop`, `check_cov`, and `get_fv_complexity`.

## Reproduction

From the repository root (`/Users/mkubavat/Silicon/git/informal-lp`):

```bash
INFORMAL_TOOLS_DIR=/Users/mkubavat/Silicon/git/informal/tools \
  python3 -I scripts/lp_paper_bench.py --exp all --repeats 3 --backend both
```

Measurements are written to the git-ignored `research/lp-paper/measurements/` directory (`env.json`, `runs.jsonl`, `exp1_pst_scaling.csv`, `ws1a_cache.csv`, `summary_tables.md`).
