"""Expected verdicts + stratified sample from the HWMCC'24 AIGER results CSV.

usage: python3 -I hwmcc_sample.py <hwmcc24 dir> <n_unsafe> <n_safe> <n_unsolved>
Writes <dir>/expected.json (all 319) and <dir>/sample60/ (symlinks).
supercar is excluded from the consensus: it is the lone 'sat' on the 8
x-epic models that ric3 and pavy prove unsat.
"""
import csv, json, os, sys

d, n_uns, n_safe, n_open = sys.argv[1], *map(int, sys.argv[2:5])
rows = list(csv.DictReader(open(os.path.join(d, 'hwmcc24_aiger.csv'))))
expected, ric3_t = {}, {}
for r in rows:
    b = r['benchmark']
    expected.setdefault(b, set())
    if r['result'] in ('sat', 'unsat') and r['solver'] != 'supercar':
        expected[b].add(r['result'])
    if r['solver'] == 'ric3':
        ric3_t[b] = float(r['time_real']) if r['result'] in ('sat', 'unsat') else None
status = {}
for b, s in expected.items():
    status[b] = {'sat': 'unsafe', 'unsat': 'safe'}[s.pop()] if len(s) == 1 else ('conflict' if s else 'unsolved')
json.dump({b: {'expected': status[b], 'ric3_s': ric3_t[b]} for b in status},
          open(os.path.join(d, 'expected.json'), 'w'), indent=1, sort_keys=True)

def spread(bs, n):
    # Even quantiles over ric3's published time (unsolved by ric3 last).
    bs = sorted(bs, key=lambda b: (ric3_t[b] is None, ric3_t[b] or 0, b))
    return [bs[round(i * (len(bs) - 1) / max(n - 1, 1))] for i in range(n)]

pick = (spread([b for b in status if status[b] == 'unsafe'], n_uns)
        + spread([b for b in status if status[b] == 'safe'], n_safe)
        + spread([b for b in status if status[b] == 'unsolved'], n_open))
out = os.path.join(d, 'sample60')
for b in pick:
    src = os.path.join(d, 'extracted', b)
    if not os.path.exists(src):
        sys.exit(f'missing {src}')
    dst = os.path.join(out, b)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if not os.path.lexists(dst):
        os.symlink(os.path.abspath(src), dst)
for b in pick:
    print(status[b], ric3_t[b], b)
