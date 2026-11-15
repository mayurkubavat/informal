"""Compare an informal run database with HWMCC'24 expected verdicts.

usage: python3 -I hwmcc_check.py <expected.json> <runs.jsonl>
Any safe/unsafe verdict that contradicts the expected status is a bug:
the exit status is 1 if there is one, 0 otherwise.
"""
import json, re, statistics, sys

exp = json.load(open(sys.argv[1]))
runs = [json.loads(l) for l in open(sys.argv[2]) if l.strip()]
bad, agree, by_engine, times = [], 0, {}, []
for r in runs:
    key = re.sub(r'^.*?/(sample60|extracted|bitlevel/safety)/', '', r['model'])
    e = exp.get(key)
    if e is None:
        continue
    s = by_engine.setdefault(r['engine'], {'solved': 0, 'agree': 0, 'unknown/err': 0, 'solved_on_unsolved': 0})
    if r['verdict'] in ('safe', 'unsafe'):
        s['solved'] += 1
        if e['expected'] == r['verdict']:
            s['agree'] += 1
        elif e['expected'] in ('safe', 'unsafe'):
            bad.append((r['engine'], key, r['verdict'], e['expected']))
        else:
            s['solved_on_unsolved'] += 1
    else:
        s['unknown/err'] += 1
    if r['engine'].startswith('ric3') and r['verdict'] in ('safe', 'unsafe') and e.get('ric3_s'):
        times.append((r['wall_ms'] / 1000, e['ric3_s'], key))
for k, v in sorted(by_engine.items()):
    print(k, v)
print('DISAGREEMENTS:', len(bad))
for b in bad:
    print('  ', *b)
if times:
    ratios = [a / b for a, b, _ in times if b > 0.05]
    print(f'ric3 local vs published (both solved, n={len(times)}): '
          f'local total {sum(a for a,_,_ in times):.1f}s, published total {sum(b for _,b,_ in times):.1f}s, '
          f'median ratio {statistics.median(ratios):.2f}')
    for a, b, k in sorted(times, key=lambda t: -t[1])[:8]:
        print(f'   {k}: local {a:.1f}s  published {b:.1f}s')
sys.exit(1 if bad else 0)
