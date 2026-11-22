//! Hungarian min-cost bipartite matcher, generic strategy bucket comparison,
//! and shared canonical normalization helpers.

use std::collections::BTreeMap;

use crate::expr_bv::{parse_logic_expr, qualify_logic_expr_to_root};
use crate::ir::{escape_rtl_brackets, Sense, StrategyLocation, UpfLoc};

use super::{UpfDiffItem, UpfDiffKind};

pub(crate) struct BucketMatchResult<'a, T> {
    pub paired_diffs: Vec<(&'a T, &'a T, Vec<String>)>,
    pub unmatched_golden: Vec<&'a T>,
    pub unmatched_revised: Vec<&'a T>,
}

pub(crate) fn hungarian_min_cost(sz: usize, edge_cost: impl Fn(usize, usize) -> i64) -> Vec<usize> {
    let mut u = vec![0i64; sz + 1];
    let mut v = vec![0i64; sz + 1];
    let mut p = vec![0usize; sz + 1];
    let mut way = vec![0usize; sz + 1];
    for i in 1..=sz {
        p[0] = i;
        let mut j0 = 0usize;
        let mut minv = vec![i64::MAX; sz + 1];
        let mut used = vec![false; sz + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = i64::MAX;
            let mut j1 = 0usize;
            for j in 1..=sz {
                if !used[j] {
                    let cur = edge_cost(i0 - 1, j - 1) - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=sz {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    p
}

pub(crate) fn match_bucket_pairs<'a, T>(
    g_list: &[&'a T],
    r_list: &[&'a T],
    name_of: impl Fn(&T) -> &str,
    mut diff_fn: impl FnMut(usize, usize) -> Vec<String>,
) -> BucketMatchResult<'a, T> {
    let n = g_list.len();
    let m = r_list.len();
    let sz = n.max(m);
    if sz == 0 {
        return BucketMatchResult {
            paired_diffs: Vec::new(),
            unmatched_golden: Vec::new(),
            unmatched_revised: Vec::new(),
        };
    }

    let mut diffs_cache: Vec<Vec<Vec<String>>> = Vec::with_capacity(n);
    for i in 0..n {
        let mut row = Vec::with_capacity(m);
        for j in 0..m {
            row.push(diff_fn(i, j));
        }
        diffs_cache.push(row);
    }

    let big_w = (sz as i64) + 1;
    let p = hungarian_min_cost(sz, |i, j| {
        if i < n && j < m && diffs_cache[i][j].is_empty() {
            let exact = i64::from(name_of(g_list[i]) == name_of(r_list[j]));
            -(big_w + exact)
        } else {
            0
        }
    });

    let mut g_used = vec![false; n];
    let mut r_used = vec![false; m];
    for j in 1..=sz {
        let i = p[j];
        if i >= 1 && i <= n && j <= m && diffs_cache[i - 1][j - 1].is_empty() {
            g_used[i - 1] = true;
            r_used[j - 1] = true;
        }
    }

    let g_rem: Vec<usize> = (0..n).filter(|&i| !g_used[i]).collect();
    let r_rem: Vec<usize> = (0..m).filter(|&j| !r_used[j]).collect();
    if g_rem.is_empty() || r_rem.is_empty() {
        return BucketMatchResult {
            paired_diffs: Vec::new(),
            unmatched_golden: g_rem.into_iter().map(|gi| g_list[gi]).collect(),
            unmatched_revised: r_rem.into_iter().map(|ri| r_list[ri]).collect(),
        };
    }

    let gn = g_rem.len();
    let rn = r_rem.len();
    let rem_sz = gn.max(rn);
    let mut max_reasons = 0i64;
    for &gi in &g_rem {
        for &ri in &r_rem {
            max_reasons = max_reasons.max(diffs_cache[gi][ri].len() as i64);
        }
    }
    let big_rem = max_reasons * 2 + 4;
    let p_rem = hungarian_min_cost(rem_sz, |a, b| {
        if a < gn && b < rn {
            let gi = g_rem[a];
            let ri = r_rem[b];
            let rlen = diffs_cache[gi][ri].len() as i64;
            let exact = i64::from(name_of(g_list[gi]) == name_of(r_list[ri]));
            (rlen * 2 - exact) - big_rem
        } else {
            0
        }
    });

    let mut rem_g_matched = vec![false; gn];
    let mut rem_r_matched = vec![false; rn];
    let mut paired_diffs = Vec::with_capacity(gn.min(rn));
    for (b, &a) in p_rem.iter().enumerate().take(rem_sz + 1).skip(1) {
        if a >= 1 && a <= gn && b <= rn {
            let ai = a - 1;
            let bi = b - 1;
            rem_g_matched[ai] = true;
            rem_r_matched[bi] = true;
            let gi = g_rem[ai];
            let ri = r_rem[bi];
            let reasons = std::mem::take(&mut diffs_cache[gi][ri]);
            paired_diffs.push((g_list[gi], r_list[ri], reasons));
        }
    }
    paired_diffs.sort_by_key(|(g, r, _)| (name_of(g), name_of(r)));
    let unmatched_golden = (0..gn)
        .filter(|&ai| !rem_g_matched[ai])
        .map(|ai| g_list[g_rem[ai]])
        .collect();
    let unmatched_revised = (0..rn)
        .filter(|&bi| !rem_r_matched[bi])
        .map(|bi| r_list[r_rem[bi]])
        .collect();
    BucketMatchResult {
        paired_diffs,
        unmatched_golden,
        unmatched_revised,
    }
}

pub(crate) type PairDiffFn<'a, T> = Box<dyn FnMut(&T, &T) -> Vec<String> + 'a>;

pub(crate) struct BucketSpec<'a, T, K> {
    pub kind: UpfDiffKind,
    pub label: &'static str,
    pub short_tag: &'static str,
    pub key_domain: fn(&K) -> &str,
    pub name_of: fn(&T) -> &str,
    pub domain_of: fn(&T) -> &str,
    pub loc_of: fn(&T) -> &Option<UpfLoc>,
    pub diff_pair: PairDiffFn<'a, T>,
}

pub(crate) fn compare_strategy_buckets<'a, T, K: Ord>(
    g_map: &BTreeMap<K, Vec<&'a T>>,
    r_map: &BTreeMap<K, Vec<&'a T>>,
    mut spec: BucketSpec<'a, T, K>,
    diffs: &mut Vec<UpfDiffItem>,
) {
    for (key, g_list) in g_map {
        let Some(r_list) = r_map.get(key) else {
            for g in g_list {
                diffs.push(UpfDiffItem {
                    kind: spec.kind,
                    subject: format!("{}:{}", (spec.domain_of)(g), (spec.name_of)(g)),
                    golden_detail: format!("{} `{}` present", spec.label, (spec.name_of)(g)),
                    revised_detail: format!("missing matching {}", spec.label),
                    golden_loc: (spec.loc_of)(g).clone(),
                    revised_loc: None,
                });
            }
            continue;
        };
        let res = match_bucket_pairs(g_list, r_list, spec.name_of, |gi, ri| {
            (spec.diff_pair)(g_list[gi], r_list[ri])
        });
        for (g, r, reasons) in res.paired_diffs {
            if !reasons.is_empty() {
                diffs.push(UpfDiffItem {
                    kind: spec.kind,
                    subject: format!("{}:{}", (spec.domain_of)(g), (spec.name_of)(g)),
                    golden_detail: format!(
                        "{} `{}`: {}",
                        spec.short_tag,
                        (spec.name_of)(g),
                        reasons.join(", ")
                    ),
                    revised_detail: format!("{} `{}`", spec.short_tag, (spec.name_of)(r)),
                    golden_loc: (spec.loc_of)(g).clone(),
                    revised_loc: (spec.loc_of)(r).clone(),
                });
            }
        }
        if g_list.len() != r_list.len() {
            diffs.push(UpfDiffItem {
                kind: spec.kind,
                subject: (spec.key_domain)(key).to_string(),
                golden_detail: format!("{} strategies", g_list.len()),
                revised_detail: format!("{} strategies", r_list.len()),
                golden_loc: None,
                revised_loc: None,
            });
            for g in res.unmatched_golden {
                diffs.push(UpfDiffItem {
                    kind: spec.kind,
                    subject: format!("{}:{}", (spec.domain_of)(g), (spec.name_of)(g)),
                    golden_detail: format!(
                        "unmatched golden {} `{}`",
                        spec.label,
                        (spec.name_of)(g)
                    ),
                    revised_detail: "no corresponding strategy in revised bucket".to_string(),
                    golden_loc: (spec.loc_of)(g).clone(),
                    revised_loc: None,
                });
            }
            for r in res.unmatched_revised {
                diffs.push(UpfDiffItem {
                    kind: spec.kind,
                    subject: format!("{}:{}", (spec.domain_of)(r), (spec.name_of)(r)),
                    golden_detail: "no corresponding strategy in golden bucket".to_string(),
                    revised_detail: format!(
                        "unmatched revised {} `{}`",
                        spec.label,
                        (spec.name_of)(r)
                    ),
                    golden_loc: None,
                    revised_loc: (spec.loc_of)(r).clone(),
                });
            }
        }
    }

    for (key, r_list) in r_map {
        if !g_map.contains_key(key) {
            for r in r_list {
                diffs.push(UpfDiffItem {
                    kind: spec.kind,
                    subject: format!("{}:{}", (spec.domain_of)(r), (spec.name_of)(r)),
                    golden_detail: format!("missing matching {}", spec.label),
                    revised_detail: format!(
                        "extra {} `{}` in revised",
                        spec.label,
                        (spec.name_of)(r)
                    ),
                    golden_loc: None,
                    revised_loc: (spec.loc_of)(r).clone(),
                });
            }
        }
    }
}

pub(crate) fn loc_compatible(a: Option<StrategyLocation>, b: Option<StrategyLocation>) -> bool {
    match (a, b) {
        (None, _) | (_, None) => true,
        (Some(StrategyLocation::Automatic), _) | (_, Some(StrategyLocation::Automatic)) => true,
        (Some(x), Some(y)) => x == y,
    }
}

pub(crate) fn norm_opt_signal(sig: Option<&str>) -> Option<String> {
    sig.map(|s| escape_rtl_brackets(s.trim_start_matches('/')))
}

pub(crate) fn norm_opt_expr(expr: Option<&str>) -> Option<String> {
    expr.map(|e| {
        let q = qualify_logic_expr_to_root(e, "");
        parse_logic_expr(&q)
            .map(|ast| format!("{ast:?}"))
            .unwrap_or(q)
    })
}

pub(crate) fn sense_rank(s: Option<Sense>) -> u8 {
    match s {
        None => 0,
        Some(Sense::High) => 1,
        Some(Sense::Low) => 2,
        Some(Sense::Posedge) => 3,
        Some(Sense::Negedge) => 4,
    }
}

pub(crate) fn loc_rank(loc: Option<StrategyLocation>) -> u8 {
    match loc {
        None => 0,
        Some(StrategyLocation::Self_) => 1,
        Some(StrategyLocation::Parent) => 2,
        Some(StrategyLocation::Sibling) => 3,
        Some(StrategyLocation::Fanout) => 4,
        Some(StrategyLocation::Automatic) => 5,
        Some(StrategyLocation::Other) => 6,
    }
}
