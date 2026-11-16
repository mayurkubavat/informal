# bench

Model lists for the HWMCC'24 benchmark runs. Only the lists are in git; the
models, frozen binaries and results stay local (see `.gitignore`).

| File | What |
| --- | --- |
| `full319.txt` | All 319 HWMCC'24 bit-level models |
| `hwmcc25-330.txt` | All 330 HWMCC'25 bit-level safety models (`scripts/hwmcc25_fetch.py` downloads them and their published answers) |
| `sample60.csv` | The 60-model sample (`scripts/hwmcc_sample.py`) |
| `genset.txt` | 75-model set for IC3 generalization experiments |
| `hwmcc-unsolved61.txt` | Models no tool solved at HWMCC'24 |
| `memout8.txt` | Models that ran out of memory at 6 GB |
| `unsafe-94f9a4b.txt` | UNSAFE verdicts re-checked after the scorr fix (the name is a commit of the pre-publication history; `ad7aace` here) |

Local only (ignored):

- `hwmcc24/` the extracted models (Zenodo 14775242) and `expected.json`
- `hwmcc25/` a link to the HWMCC'25 models and `expected.json` (Zenodo 17428464, CC-BY-4.0), made by `scripts/hwmcc25_fetch.py`
- `bin/` frozen `informal-<commit>` binaries used by long runs
- `runs-*.jsonl` run databases, `*.log` run logs
- `measure/` runs of `scripts/measure.py`

## Measurements for publication

`scripts/measure.py` is the harness for numbers that are published:

```sh
python3 -I scripts/measure.py freeze             # bench/bin/informal-<commit>
python3 -I scripts/measure.py run --binary bench/bin/informal-<commit> \
    --models bench/full319.txt --config portfolio --timeout 3600 --mem 20G --repeats 3
python3 -I scripts/measure.py table bench/measure/<run> [<run> ...] \
    --expected bench/hwmcc24/expected.json --format md   # or csv, tex
```

On the Linux box a run waits in the job queue (`dvq`, see
`~/.claude/CLAUDE.md` there) as a `formal` job that reserves every CPU, so
no other queued work starts beside it; `DVQ=` runs it directly. Each model
runs in its own scope in `rdev.slice`, where the queue counts it, with a
hard memory limit and no swap. Before each model the harness measures how much CPU is in use
outside it and waits until that is under `--max-outside` (default 10% of
the CPUs), and it refuses to start on a busy machine; each row records the
value. `--config` is `portfolio` or one engine of `config/engines.toml`
(`ric3`, `informal-ic3`, ...). A run directory under `bench/measure/` holds
`manifest.json` (binary hash, `tools.lock`, machine, limits, model list
hash) and `runs.jsonl`; the same command resumes an interrupted run and
refuses to mix settings. `table` prints, per configuration, the median
number solved over the repeats with the lowest and highest, PAR-2, the
models only that configuration solves, and every verdict that contradicts
the published answer (the exit status is 1 if there is one).

## Day-to-day runs

Run with `scripts/hwmcc_full.sh`, `scripts/inhouse_sample.sh` or
`scripts/inhouse_capped.sh`; score with `python3 -I scripts/hwmcc_check.py`.
Run databases written before the InFormal rename name in-house engines
`fdv-*` (now `informal-*`).
