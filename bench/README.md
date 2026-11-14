# bench

Model lists for the HWMCC'24 benchmark runs. Only the lists are in git; the
models, frozen binaries and results stay local (see `.gitignore`).

| File | What |
| --- | --- |
| `full319.txt` | All 319 HWMCC'24 bit-level models |
| `sample60.csv` | The 60-model sample (`scripts/hwmcc_sample.py`) |
| `genset.txt` | 75-model set for IC3 generalization experiments |
| `hwmcc-unsolved61.txt` | Models no tool solved at HWMCC'24 |
| `memout8.txt` | Models that ran out of memory at 6 GB |
| `unsafe-94f9a4b.txt` | UNSAFE verdicts re-checked after the scorr fix (the name is a commit of the pre-publication history; `ad7aace` here) |

Local only (ignored):

- `hwmcc24/` the extracted models (Zenodo 14775242) and `expected.json`
- `bin/` frozen `informal-<commit>` binaries used by long runs
- `runs-*.jsonl` run databases, `*.log` run logs

Run with `scripts/hwmcc_full.sh`, `scripts/inhouse_sample.sh` or
`scripts/inhouse_capped.sh`; score with `python3 -I scripts/hwmcc_check.py`.
Run databases written before the InFormal rename name in-house engines
`fdv-*` (now `informal-*`).
