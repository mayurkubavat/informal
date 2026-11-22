//! Per-crossing isolation, level-shifter, and retention strategy compatibility checks.

use std::collections::BTreeMap;

use crate::ir::{
    AppliesTo, ClampValue, IsolationStrategy, LevelShifterStrategy, LsRule, RetentionStrategy,
    SupplySetId,
};
use crate::pst::PstSat;
use crate::resolve::{lookup_scoped, resolve_ss_handle, ResolvedPowerIntent};

use super::bipartite::{
    compare_strategy_buckets, loc_compatible, loc_rank, norm_opt_expr, norm_opt_signal, sense_rank,
    BucketSpec,
};
use super::domains_pst::{canonical_domain_name, canonical_elements, canonical_filter_ref};
use super::{UpfDiffItem, UpfDiffKind};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IsoKey {
    domain: String,
    applies_to: String,
    source_filter: Option<String>,
    sink_filter: Option<String>,
    elements: Vec<String>,
    exclude_elements: Vec<String>,
}

fn iso_key(resolved: &ResolvedPowerIntent, iso: &IsolationStrategy) -> IsoKey {
    let app = match iso.applies_to.unwrap_or(AppliesTo::Outputs) {
        AppliesTo::Inputs => "inputs",
        AppliesTo::Outputs => "outputs",
        AppliesTo::Both => "both",
    };
    IsoKey {
        domain: canonical_domain_name(resolved, &iso.domain, &iso.scope),
        applies_to: app.to_string(),
        source_filter: iso
            .source_filter
            .as_deref()
            .map(|f| canonical_filter_ref(resolved, f, &iso.scope)),
        sink_filter: iso
            .sink_filter
            .as_deref()
            .map(|f| canonical_filter_ref(resolved, f, &iso.scope)),
        elements: canonical_elements(&iso.elements),
        exclude_elements: canonical_elements(&iso.exclude_elements),
    }
}

fn clamp_rank(c: Option<ClampValue>) -> u8 {
    match c {
        None => 0,
        Some(ClampValue::Zero) => 1,
        Some(ClampValue::One) => 2,
        Some(ClampValue::Latch) => 3,
        Some(ClampValue::Any) => 4,
        Some(ClampValue::Z) => 5,
    }
}

fn iso_sort_key(
    iso: &IsolationStrategy,
) -> (bool, u8, u8, bool, bool, u8, Option<String>, u8, &str) {
    (
        iso.no_isolation,
        clamp_rank(iso.clamp_value),
        clamp_rank(iso.applies_to_clamp),
        iso.diff_supply_only,
        iso.force_isolation,
        sense_rank(iso.isolation_sense),
        norm_opt_signal(iso.isolation_signal.as_deref()),
        loc_rank(iso.location),
        iso.name.as_str(),
    )
}

fn iso_supply_adequate(
    resolved: &ResolvedPowerIntent,
    sat: &mut PstSat,
    iso: &IsolationStrategy,
    iso_ss: SupplySetId,
    pd_ss: SupplySetId,
) -> bool {
    if iso.no_isolation {
        return true;
    }
    if sat.is_supply_always_on_when(iso_ss, pd_ss).is_err() {
        return false;
    }
    if iso.applies_to.unwrap_or(AppliesTo::Outputs) == AppliesTo::Inputs {
        return true;
    }
    let mut partner_sets: Vec<SupplySetId> = Vec::new();
    if let Some(sink) = &iso.sink_filter {
        if let Some(pd_id) = lookup_scoped(&resolved.domain_by_name, sink, &iso.scope) {
            if let Some(ss) = resolved.primary_ss(pd_id) {
                partner_sets.push(ss);
            }
        } else if let Some(ss) = resolve_ss_handle(
            sink,
            &iso.scope,
            &resolved.intent.domains,
            &resolved.domain_by_name,
            &resolved.ss_by_name,
        ) {
            partner_sets.push(ss);
        }
    }
    if partner_sets.is_empty() {
        for pd in &resolved.intent.domains {
            if let Some(other_ss) = resolved.primary_ss(pd.id) {
                if other_ss != pd_ss && sat.can_be_off_while_on(pd_ss, other_ss).is_some() {
                    partner_sets.push(other_ss);
                }
            }
        }
    }
    if partner_sets.is_empty() {
        if iso.sink_filter.is_some() {
            return true;
        }
        return sat
            .is_supply_always_on_when_corrupting(iso_ss, pd_ss)
            .is_ok();
    }
    partner_sets.into_iter().all(|other_ss| {
        sat.is_supply_always_on_when_crossing(iso_ss, pd_ss, other_ss)
            .is_ok()
    })
}

fn iso_strategy_supply_ok(
    resolved: &ResolvedPowerIntent,
    sat: &mut PstSat,
    iso: &IsolationStrategy,
) -> bool {
    if iso.no_isolation {
        return true;
    }
    let Some(iso_ss) = resolved.effective_iso_supply(iso) else {
        return false;
    };
    let Some(pd_ss) = lookup_scoped(&resolved.domain_by_name, &iso.domain, &iso.scope)
        .and_then(|id| resolved.primary_ss(id))
    else {
        return false;
    };
    iso_supply_adequate(resolved, sat, iso, iso_ss, pd_ss)
}

fn iso_diff_reasons(
    g: &IsolationStrategy,
    r: &IsolationStrategy,
    g_ok: bool,
    r_ok: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if g.no_isolation != r.no_isolation {
        reasons.push(format!(
            "no_isolation ({} vs {})",
            g.no_isolation, r.no_isolation
        ));
    }
    if g.clamp_value != r.clamp_value {
        reasons.push(format!(
            "clamp_value ({:?} vs {:?})",
            g.clamp_value, r.clamp_value
        ));
    }
    if g.applies_to_clamp != r.applies_to_clamp {
        reasons.push(format!(
            "applies_to_clamp ({:?} vs {:?})",
            g.applies_to_clamp, r.applies_to_clamp
        ));
    }
    if g.diff_supply_only != r.diff_supply_only {
        reasons.push(format!(
            "diff_supply_only ({} vs {})",
            g.diff_supply_only, r.diff_supply_only
        ));
    }
    if g.force_isolation != r.force_isolation {
        reasons.push(format!(
            "force_isolation ({} vs {})",
            g.force_isolation, r.force_isolation
        ));
    }
    if g.isolation_sense != r.isolation_sense {
        reasons.push(format!(
            "isolation_sense ({:?} vs {:?})",
            g.isolation_sense, r.isolation_sense
        ));
    }
    if norm_opt_signal(g.isolation_signal.as_deref())
        != norm_opt_signal(r.isolation_signal.as_deref())
    {
        reasons.push(format!(
            "isolation_signal ({:?} vs {:?})",
            g.isolation_signal, r.isolation_signal
        ));
    }
    if !loc_compatible(g.location, r.location) {
        reasons.push(format!("location ({:?} vs {:?})", g.location, r.location));
    }
    if !g.no_isolation && !r.no_isolation && g_ok != r_ok {
        reasons.push(if !g_ok {
            "golden isolation supply is not always-on when needed (domain OFF or ON)".to_string()
        } else {
            "revised isolation supply is not always-on when needed (domain OFF or ON)".to_string()
        });
    }
    reasons
}

pub(crate) fn compare_isolation_strategies(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    g_sat: &mut PstSat,
    r_sat: &mut PstSat,
    diffs: &mut Vec<UpfDiffItem>,
) {
    let mut g_map: BTreeMap<IsoKey, Vec<&IsolationStrategy>> = BTreeMap::new();
    for iso in &golden.intent.isolation_strategies {
        g_map.entry(iso_key(golden, iso)).or_default().push(iso);
    }
    for list in g_map.values_mut() {
        list.sort_by_key(|iso| iso_sort_key(iso));
    }
    let mut r_map: BTreeMap<IsoKey, Vec<&IsolationStrategy>> = BTreeMap::new();
    for iso in &revised.intent.isolation_strategies {
        r_map.entry(iso_key(revised, iso)).or_default().push(iso);
    }
    for list in r_map.values_mut() {
        list.sort_by_key(|iso| iso_sort_key(iso));
    }

    let mut g_ok_cache = BTreeMap::new();
    for g_list in g_map.values() {
        for iso in g_list {
            let ok = iso_strategy_supply_ok(golden, g_sat, iso);
            g_ok_cache.insert(iso.id.0, ok);
        }
    }
    let mut r_ok_cache = BTreeMap::new();
    for r_list in r_map.values() {
        for iso in r_list {
            let ok = iso_strategy_supply_ok(revised, r_sat, iso);
            r_ok_cache.insert(iso.id.0, ok);
        }
    }

    compare_strategy_buckets(
        &g_map,
        &r_map,
        BucketSpec {
            kind: UpfDiffKind::IsolationStrategyMismatch,
            label: "isolation strategy",
            short_tag: "iso",
            key_domain: |k: &IsoKey| k.domain.as_str(),
            name_of: |iso: &IsolationStrategy| iso.name.as_str(),
            domain_of: |iso: &IsolationStrategy| iso.domain.as_str(),
            loc_of: |iso: &IsolationStrategy| &iso.loc,
            diff_pair: Box::new(move |g, r| {
                iso_diff_reasons(g, r, g_ok_cache[&g.id.0], r_ok_cache[&r.id.0])
            }),
        },
        diffs,
    );
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LsKey {
    domain: String,
    applies_to: String,
    source_filter: Option<String>,
    sink_filter: Option<String>,
    elements: Vec<String>,
    exclude_elements: Vec<String>,
}

fn ls_key(resolved: &ResolvedPowerIntent, ls: &LevelShifterStrategy) -> LsKey {
    let app = match ls.applies_to.unwrap_or(AppliesTo::Both) {
        AppliesTo::Inputs => "inputs",
        AppliesTo::Outputs => "outputs",
        AppliesTo::Both => "both",
    };
    LsKey {
        domain: canonical_domain_name(resolved, &ls.domain, &ls.scope),
        applies_to: app.to_string(),
        source_filter: ls
            .source_filter
            .as_deref()
            .map(|f| canonical_filter_ref(resolved, f, &ls.scope)),
        sink_filter: ls
            .sink_filter
            .as_deref()
            .map(|f| canonical_filter_ref(resolved, f, &ls.scope)),
        elements: canonical_elements(&ls.elements),
        exclude_elements: canonical_elements(&ls.exclude_elements),
    }
}

struct SsNetInfo<'a> {
    name: &'a str,
    power: Option<&'a str>,
    ground: Option<&'a str>,
}

fn resolve_ss_info<'a>(
    resolved: &'a ResolvedPowerIntent,
    raw: &str,
    sc: &str,
) -> Option<SsNetInfo<'a>> {
    resolve_ss_handle(
        raw,
        sc,
        &resolved.intent.domains,
        &resolved.domain_by_name,
        &resolved.ss_by_name,
    )
    .map(|id| {
        let s = &resolved.intent.supply_sets[id.0 as usize];
        let power = s.power.as_deref().and_then(|n| {
            lookup_scoped(&resolved.net_by_name, n, &s.scope)
                .map(|nid| resolved.intent.supply_nets[nid.0 as usize].name.as_str())
        });
        let ground = s.ground.as_deref().and_then(|n| {
            lookup_scoped(&resolved.net_by_name, n, &s.scope)
                .map(|nid| resolved.intent.supply_nets[nid.0 as usize].name.as_str())
        });
        SsNetInfo {
            name: s.name.as_str(),
            power,
            ground,
        }
    })
}

fn supply_ref_eq(
    golden: &ResolvedPowerIntent,
    g_ref: Option<&str>,
    g_scope: &str,
    revised: &ResolvedPowerIntent,
    r_ref: Option<&str>,
    r_scope: &str,
) -> bool {
    match (g_ref, r_ref) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(gs), Some(rs)) => match (
            resolve_ss_info(golden, gs, g_scope),
            resolve_ss_info(revised, rs, r_scope),
        ) {
            (Some(g_info), Some(r_info)) => {
                if g_info.power.is_some()
                    || g_info.ground.is_some()
                    || r_info.power.is_some()
                    || r_info.ground.is_some()
                {
                    (g_info.power, g_info.ground) == (r_info.power, r_info.ground)
                } else {
                    g_info.name == r_info.name
                }
            }
            _ => false,
        },
    }
}

fn ls_sort_key(ls: &LevelShifterStrategy) -> (bool, u8, Option<i64>, bool, u8, &str) {
    let rule_r = match ls.rule {
        None => 0,
        Some(LsRule::LowToHigh) => 1,
        Some(LsRule::HighToLow) => 2,
        Some(LsRule::Both) => 3,
    };
    (
        ls.no_shift,
        rule_r,
        ls.threshold_uv,
        ls.force_shift,
        loc_rank(ls.location),
        ls.name.as_str(),
    )
}

fn ls_diff_reasons(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    g: &LevelShifterStrategy,
    r: &LevelShifterStrategy,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if g.no_shift != r.no_shift {
        reasons.push(format!("no_shift ({} vs {})", g.no_shift, r.no_shift));
    }
    if g.rule != r.rule {
        reasons.push(format!("rule ({:?} vs {:?})", g.rule, r.rule));
    }
    if !loc_compatible(g.location, r.location) {
        reasons.push(format!("location ({:?} vs {:?})", g.location, r.location));
    }
    if g.threshold_uv != r.threshold_uv {
        reasons.push(format!(
            "threshold_uv ({:?} vs {:?})",
            g.threshold_uv, r.threshold_uv
        ));
    }
    if g.force_shift != r.force_shift {
        reasons.push(format!(
            "force_shift ({} vs {})",
            g.force_shift, r.force_shift
        ));
    }
    if !supply_ref_eq(
        golden,
        g.input_supply_set.as_deref(),
        &g.scope,
        revised,
        r.input_supply_set.as_deref(),
        &r.scope,
    ) {
        reasons.push(format!(
            "input_supply_set ({:?} vs {:?})",
            g.input_supply_set, r.input_supply_set
        ));
    }
    if !supply_ref_eq(
        golden,
        g.output_supply_set.as_deref(),
        &g.scope,
        revised,
        r.output_supply_set.as_deref(),
        &r.scope,
    ) {
        reasons.push(format!(
            "output_supply_set ({:?} vs {:?})",
            g.output_supply_set, r.output_supply_set
        ));
    }
    if !supply_ref_eq(
        golden,
        g.internal_supply_set.as_deref(),
        &g.scope,
        revised,
        r.internal_supply_set.as_deref(),
        &r.scope,
    ) {
        reasons.push(format!(
            "internal_supply_set ({:?} vs {:?})",
            g.internal_supply_set, r.internal_supply_set
        ));
    }
    reasons
}

pub(crate) fn compare_level_shifters(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    diffs: &mut Vec<UpfDiffItem>,
) {
    let mut g_map: BTreeMap<LsKey, Vec<&LevelShifterStrategy>> = BTreeMap::new();
    for ls in &golden.intent.level_shifter_strategies {
        g_map.entry(ls_key(golden, ls)).or_default().push(ls);
    }
    for list in g_map.values_mut() {
        list.sort_by_key(|ls| ls_sort_key(ls));
    }
    let mut r_map: BTreeMap<LsKey, Vec<&LevelShifterStrategy>> = BTreeMap::new();
    for ls in &revised.intent.level_shifter_strategies {
        r_map.entry(ls_key(revised, ls)).or_default().push(ls);
    }
    for list in r_map.values_mut() {
        list.sort_by_key(|ls| ls_sort_key(ls));
    }

    compare_strategy_buckets(
        &g_map,
        &r_map,
        BucketSpec {
            kind: UpfDiffKind::LevelShifterStrategyMismatch,
            label: "level shifter",
            short_tag: "ls",
            key_domain: |k: &LsKey| k.domain.as_str(),
            name_of: |ls: &LevelShifterStrategy| ls.name.as_str(),
            domain_of: |ls: &LevelShifterStrategy| ls.domain.as_str(),
            loc_of: |ls: &LevelShifterStrategy| &ls.loc,
            diff_pair: Box::new(|g, r| ls_diff_reasons(golden, revised, g, r)),
        },
        diffs,
    );
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RetKey {
    domain: String,
    elements: Vec<String>,
    exclude_elements: Vec<String>,
}

fn ret_key(resolved: &ResolvedPowerIntent, ret: &RetentionStrategy) -> RetKey {
    RetKey {
        domain: canonical_domain_name(resolved, &ret.domain, &ret.scope),
        elements: canonical_elements(&ret.elements),
        exclude_elements: canonical_elements(&ret.exclude_elements),
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RetSortKey<'a> {
    no_retention: bool,
    save_signal: Option<String>,
    save_sense: u8,
    restore_signal: Option<String>,
    restore_sense: u8,
    save_condition: Option<String>,
    restore_condition: Option<String>,
    retention_condition: Option<String>,
    use_retention_as_primary: bool,
    name: &'a str,
}

fn ret_sort_key(ret: &RetentionStrategy) -> RetSortKey<'_> {
    RetSortKey {
        no_retention: ret.no_retention,
        save_signal: norm_opt_signal(ret.save_signal.as_deref()),
        save_sense: sense_rank(ret.save_sense),
        restore_signal: norm_opt_signal(ret.restore_signal.as_deref()),
        restore_sense: sense_rank(ret.restore_sense),
        save_condition: norm_opt_expr(ret.save_condition.as_deref()),
        restore_condition: norm_opt_expr(ret.restore_condition.as_deref()),
        retention_condition: norm_opt_expr(ret.retention_condition.as_deref()),
        use_retention_as_primary: ret.use_retention_as_primary,
        name: ret.name.as_str(),
    }
}

fn ret_strategy_always_on(
    resolved: &ResolvedPowerIntent,
    sat: &mut PstSat,
    ret: &RetentionStrategy,
) -> bool {
    if ret.no_retention {
        return true;
    }
    let Some(ret_ss) = resolved.effective_ret_supply(ret) else {
        return false;
    };
    let Some(pd_ss) = lookup_scoped(&resolved.domain_by_name, &ret.domain, &ret.scope)
        .and_then(|id| resolved.primary_ss(id))
    else {
        return false;
    };
    !resolved.same_func_supply_class(ret_ss, pd_ss)
        && sat.is_supply_always_on_when(ret_ss, pd_ss).is_ok()
        && sat.can_be_non_normal_while_off(ret_ss, pd_ss).is_none()
        && sat.can_corrupt_state_while_on(pd_ss, ret_ss).is_some()
}

fn ret_diff_reasons(
    g: &RetentionStrategy,
    r: &RetentionStrategy,
    g_always_on: bool,
    r_always_on: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if g.no_retention != r.no_retention {
        reasons.push(format!(
            "no_retention ({} vs {})",
            g.no_retention, r.no_retention
        ));
    }
    if g.save_sense != r.save_sense
        || norm_opt_signal(g.save_signal.as_deref()) != norm_opt_signal(r.save_signal.as_deref())
    {
        reasons.push(format!(
            "save ({:?}/{:?} vs {:?}/{:?})",
            g.save_signal, g.save_sense, r.save_signal, r.save_sense
        ));
    }
    if g.restore_sense != r.restore_sense
        || norm_opt_signal(g.restore_signal.as_deref())
            != norm_opt_signal(r.restore_signal.as_deref())
    {
        reasons.push(format!(
            "restore ({:?}/{:?} vs {:?}/{:?})",
            g.restore_signal, g.restore_sense, r.restore_signal, r.restore_sense
        ));
    }
    if norm_opt_expr(g.save_condition.as_deref()) != norm_opt_expr(r.save_condition.as_deref()) {
        reasons.push(format!(
            "save_condition ({:?} vs {:?})",
            g.save_condition, r.save_condition
        ));
    }
    if norm_opt_expr(g.restore_condition.as_deref())
        != norm_opt_expr(r.restore_condition.as_deref())
    {
        reasons.push(format!(
            "restore_condition ({:?} vs {:?})",
            g.restore_condition, r.restore_condition
        ));
    }
    if norm_opt_expr(g.retention_condition.as_deref())
        != norm_opt_expr(r.retention_condition.as_deref())
    {
        reasons.push(format!(
            "retention_condition ({:?} vs {:?})",
            g.retention_condition, r.retention_condition
        ));
    }
    if g.use_retention_as_primary != r.use_retention_as_primary {
        reasons.push(format!(
            "use_retention_as_primary ({} vs {})",
            g.use_retention_as_primary, r.use_retention_as_primary
        ));
    }
    if !g.no_retention && !r.no_retention && g_always_on != r_always_on {
        reasons.push(if !g_always_on {
            "golden retention supply is not always-on relative to domain primary supply".to_string()
        } else {
            "revised retention supply is not always-on relative to domain primary supply"
                .to_string()
        });
    }
    reasons
}

pub(crate) fn compare_retention_strategies(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    g_sat: &mut PstSat,
    r_sat: &mut PstSat,
    diffs: &mut Vec<UpfDiffItem>,
) {
    let mut g_map: BTreeMap<RetKey, Vec<&RetentionStrategy>> = BTreeMap::new();
    for ret in &golden.intent.retention_strategies {
        g_map.entry(ret_key(golden, ret)).or_default().push(ret);
    }
    for list in g_map.values_mut() {
        list.sort_by_key(|ret| ret_sort_key(ret));
    }
    let mut r_map: BTreeMap<RetKey, Vec<&RetentionStrategy>> = BTreeMap::new();
    for ret in &revised.intent.retention_strategies {
        r_map.entry(ret_key(revised, ret)).or_default().push(ret);
    }
    for list in r_map.values_mut() {
        list.sort_by_key(|ret| ret_sort_key(ret));
    }

    let mut g_ok_cache = BTreeMap::new();
    for g_list in g_map.values() {
        for ret in g_list {
            let ok = ret_strategy_always_on(golden, g_sat, ret);
            g_ok_cache.insert(ret.id.0, ok);
        }
    }
    let mut r_ok_cache = BTreeMap::new();
    for r_list in r_map.values() {
        for ret in r_list {
            let ok = ret_strategy_always_on(revised, r_sat, ret);
            r_ok_cache.insert(ret.id.0, ok);
        }
    }

    compare_strategy_buckets(
        &g_map,
        &r_map,
        BucketSpec {
            kind: UpfDiffKind::RetentionStrategyMismatch,
            label: "retention strategy",
            short_tag: "ret",
            key_domain: |k: &RetKey| k.domain.as_str(),
            name_of: |ret: &RetentionStrategy| ret.name.as_str(),
            domain_of: |ret: &RetentionStrategy| ret.domain.as_str(),
            loc_of: |ret: &RetentionStrategy| &ret.loc,
            diff_pair: Box::new(move |g, r| {
                ret_diff_reasons(g, r, g_ok_cache[&g.id.0], r_ok_cache[&r.id.0])
            }),
        },
        diffs,
    );
}
