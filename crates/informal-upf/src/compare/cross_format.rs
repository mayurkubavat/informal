//! Cross-format (UPF vs CPF) semantic level-shifter equivalence comparison.

use std::collections::BTreeMap;

use crate::ir::{AppliesTo, LevelShifterStrategy, LsRule, PowerDomainId};
use crate::pst::PstSat;
use crate::resolve::ResolvedPowerIntent;

use super::domains_pst::{canonical_domain_name, canonical_elements};
use super::{UpfDiffItem, UpfDiffKind};

fn ls_rule_compat(a: Option<LsRule>, b: Option<LsRule>) -> bool {
    !matches!(
        (a, b),
        (Some(LsRule::LowToHigh), Some(LsRule::HighToLow))
            | (Some(LsRule::HighToLow), Some(LsRule::LowToHigh))
    )
}

fn ls_applies_to_pair(
    resolved: &ResolvedPowerIntent,
    ls: &LevelShifterStrategy,
    src: &str,
    dst: &str,
    req_tag: &str,
) -> bool {
    let dom = canonical_domain_name(resolved, &ls.domain, &ls.scope);
    let eff = ls.applies_to.unwrap_or(AppliesTo::Both);
    let dir_ok = (matches!(eff, AppliesTo::Outputs | AppliesTo::Both) && dom == src)
        || (matches!(eff, AppliesTo::Inputs | AppliesTo::Both) && dom == dst);
    if !dir_ok {
        return false;
    }
    if let Some(sf) = ls.source_filter.as_deref() {
        if canonical_domain_name(resolved, sf, &ls.scope) != src {
            return false;
        }
    }
    if let Some(df) = ls.sink_filter.as_deref() {
        if canonical_domain_name(resolved, df, &ls.scope) != dst {
            return false;
        }
    }
    if ls.no_shift {
        return true;
    }
    !matches!(
        (req_tag, ls.rule),
        ("low_to_high", Some(LsRule::HighToLow)) | ("high_to_low", Some(LsRule::LowToHigh))
    )
}

pub(crate) fn compare_level_shifters_cross_format(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    matched_domains: &[(String, PowerDomainId, PowerDomainId)],
    g_sat: &mut PstSat,
    tol_uv: i64,
    diffs: &mut Vec<UpfDiffItem>,
) {
    let mut g_elems: BTreeMap<String, Vec<&LevelShifterStrategy>> = BTreeMap::new();
    let mut r_elems: BTreeMap<String, Vec<&LevelShifterStrategy>> = BTreeMap::new();
    let mut g_wide: Vec<&LevelShifterStrategy> = Vec::new();
    let mut r_wide: Vec<&LevelShifterStrategy> = Vec::new();
    for ls in &golden.intent.level_shifter_strategies {
        if ls.elements.is_empty() {
            g_wide.push(ls);
        } else {
            for e in canonical_elements(&ls.elements) {
                g_elems.entry(e).or_default().push(ls);
            }
        }
    }
    for ls in &revised.intent.level_shifter_strategies {
        if ls.elements.is_empty() {
            r_wide.push(ls);
        } else {
            for e in canonical_elements(&ls.elements) {
                r_elems.entry(e).or_default().push(ls);
            }
        }
    }
    for (el, gl) in &g_elems {
        match r_elems.get(el) {
            None => diffs.push(UpfDiffItem {
                kind: UpfDiffKind::LevelShifterStrategyMismatch,
                subject: el.clone(),
                golden_detail: format!("level shifter `{}` covers element `{el}`", gl[0].name),
                revised_detail: "missing matching level shifter element in revised".to_string(),
                golden_loc: gl[0].loc.clone(),
                revised_loc: None,
            }),
            Some(rl) => {
                if !gl.iter().any(|g| {
                    rl.iter()
                        .any(|r| g.no_shift == r.no_shift && ls_rule_compat(g.rule, r.rule))
                }) {
                    diffs.push(UpfDiffItem {
                        kind: UpfDiffKind::LevelShifterStrategyMismatch,
                        subject: el.clone(),
                        golden_detail: format!(
                            "rule={:?}, no_shift={}",
                            gl[0].rule, gl[0].no_shift
                        ),
                        revised_detail: format!(
                            "rule={:?}, no_shift={}",
                            rl[0].rule, rl[0].no_shift
                        ),
                        golden_loc: gl[0].loc.clone(),
                        revised_loc: rl[0].loc.clone(),
                    });
                }
            }
        }
    }
    for (el, rl) in &r_elems {
        if !g_elems.contains_key(el) {
            diffs.push(UpfDiffItem {
                kind: UpfDiffKind::LevelShifterStrategyMismatch,
                subject: el.clone(),
                golden_detail: "missing matching level shifter element in golden".to_string(),
                revised_detail: format!("extra level shifter `{}` for element `{el}`", rl[0].name),
                golden_loc: None,
                revised_loc: rl[0].loc.clone(),
            });
        }
    }
    if g_wide.is_empty() && r_wide.is_empty() {
        return;
    }
    let mut shift_pairs: Vec<(&str, &str, &'static str)> = Vec::new();
    for (sn, gs, _) in matched_domains {
        for (dn, gd, _) in matched_domains {
            if sn == dn {
                continue;
            }
            if let (Some(ss), Some(ds)) = (golden.primary_ss(*gs), golden.primary_ss(*gd)) {
                let tag = g_sat.voltage_relation(ss, ds, tol_uv).kind_tag();
                if matches!(tag, "low_to_high" | "high_to_low" | "both") {
                    shift_pairs.push((sn.as_str(), dn.as_str(), tag));
                }
            }
        }
    }
    for g in &g_wide {
        let covered = r_wide.iter().any(|r| {
            g.no_shift == r.no_shift
                && ls_rule_compat(g.rule, r.rule)
                && shift_pairs.iter().any(|&(s, d, t)| {
                    ls_applies_to_pair(golden, g, s, d, t)
                        && ls_applies_to_pair(revised, r, s, d, t)
                })
        });
        if !covered {
            diffs.push(UpfDiffItem {
                kind: UpfDiffKind::LevelShifterStrategyMismatch,
                subject: format!("{}:{}", g.domain, g.name),
                golden_detail: format!("unmatched golden level shifter `{}`", g.name),
                revised_detail: "no compatible level shifter in revised".to_string(),
                golden_loc: g.loc.clone(),
                revised_loc: None,
            });
        }
    }
    for r in &r_wide {
        let covered = g_wide.iter().any(|g| {
            g.no_shift == r.no_shift
                && ls_rule_compat(g.rule, r.rule)
                && shift_pairs.iter().any(|&(s, d, t)| {
                    ls_applies_to_pair(golden, g, s, d, t)
                        && ls_applies_to_pair(revised, r, s, d, t)
                })
        });
        if !covered {
            diffs.push(UpfDiffItem {
                kind: UpfDiffKind::LevelShifterStrategyMismatch,
                subject: format!("{}:{}", r.domain, r.name),
                golden_detail: "no compatible level shifter in golden".to_string(),
                revised_detail: format!("extra revised level shifter `{}`", r.name),
                golden_loc: None,
                revised_loc: r.loc.clone(),
            });
        }
    }
}
