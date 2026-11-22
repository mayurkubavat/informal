//! Domain partition, supply-equivalence alignment, and symbolic PST crossing-matrix comparison.

use crate::ir::{escape_rtl_brackets, PowerDomainId, SupplySetId};
use crate::pst::PstSat;
use crate::resolve::{lookup_scoped, resolve_ss_handle, ResolvedPowerIntent};
use crate::upf::ObjectFinder;

use super::{UpfDiffItem, UpfDiffKind};

pub(crate) fn compare_domain_partitions(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    finder: &dyn ObjectFinder,
    diffs: &mut Vec<UpfDiffItem>,
) -> Vec<(String, PowerDomainId, PowerDomainId)> {
    let mut matched_domains: Vec<(String, PowerDomainId, PowerDomainId)> = Vec::new();
    for (name, &g_id) in &golden.domain_by_name {
        if let Some(&r_id) = revised.domain_by_name.get(name) {
            matched_domains.push((name.clone(), g_id, r_id));

            let g_pd = &golden.intent.domains[g_id.0 as usize];
            let r_pd = &revised.intent.domains[r_id.0 as usize];
            if finder.has_design() && golden.has_design && revised.has_design {
                let g_insts = &golden.domain_instances[g_id.0 as usize];
                let r_insts = &revised.domain_instances[r_id.0 as usize];
                if g_insts != r_insts {
                    diffs.push(UpfDiffItem {
                        kind: UpfDiffKind::DomainMembershipMismatch,
                        subject: name.clone(),
                        golden_detail: format!("instances={g_insts:?}"),
                        revised_detail: format!("instances={r_insts:?}"),
                        golden_loc: g_pd.loc.clone(),
                        revised_loc: r_pd.loc.clone(),
                    });
                }
            } else {
                let g_elems = canonical_elements(&g_pd.elements);
                let r_elems = canonical_elements(&r_pd.elements);
                let g_excl = canonical_elements(&g_pd.exclude_elements);
                let r_excl = canonical_elements(&r_pd.exclude_elements);
                if g_elems != r_elems
                    || g_excl != r_excl
                    || g_pd.include_scope != r_pd.include_scope
                {
                    diffs.push(UpfDiffItem {
                        kind: UpfDiffKind::DomainMembershipMismatch,
                        subject: name.clone(),
                        golden_detail: format!(
                            "include_scope={}, elements={g_elems:?}, exclude={g_excl:?}",
                            g_pd.include_scope
                        ),
                        revised_detail: format!(
                            "include_scope={}, elements={r_elems:?}, exclude={r_excl:?}",
                            r_pd.include_scope
                        ),
                        golden_loc: g_pd.loc.clone(),
                        revised_loc: r_pd.loc.clone(),
                    });
                }
            }
        } else {
            let g_pd = &golden.intent.domains[g_id.0 as usize];
            diffs.push(UpfDiffItem {
                kind: UpfDiffKind::MissingDomain,
                subject: name.clone(),
                golden_detail: format!("domain `{name}` present in golden"),
                revised_detail: "missing in revised".to_string(),
                golden_loc: g_pd.loc.clone(),
                revised_loc: None,
            });
        }
    }

    for (name, &r_id) in &revised.domain_by_name {
        if !golden.domain_by_name.contains_key(name) {
            let r_pd = &revised.intent.domains[r_id.0 as usize];
            diffs.push(UpfDiffItem {
                kind: UpfDiffKind::ExtraDomain,
                subject: name.clone(),
                golden_detail: "missing in golden".to_string(),
                revised_detail: format!("extra domain `{name}` in revised"),
                golden_loc: None,
                revised_loc: r_pd.loc.clone(),
            });
        }
    }

    matched_domains
}

pub(crate) fn compare_supply_equivalence(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    matched_domains: &[(String, PowerDomainId, PowerDomainId)],
    diffs: &mut Vec<UpfDiffItem>,
) {
    for i in 0..matched_domains.len() {
        for j in (i + 1)..matched_domains.len() {
            let (ref na, ga, ra) = matched_domains[i];
            let (ref nb, gb, rb) = matched_domains[j];
            let g_eq = match (golden.primary_ss(ga), golden.primary_ss(gb)) {
                (Some(sa), Some(sb)) => golden.same_supply_class(sa, sb),
                _ => false,
            };
            let r_eq = match (revised.primary_ss(ra), revised.primary_ss(rb)) {
                (Some(sa), Some(sb)) => revised.same_supply_class(sa, sb),
                _ => false,
            };
            if g_eq != r_eq {
                diffs.push(UpfDiffItem {
                    kind: UpfDiffKind::SupplyEquivalenceMismatch,
                    subject: format!("{na} <-> {nb}"),
                    golden_detail: format!("equivalent={g_eq}"),
                    revised_detail: format!("equivalent={r_eq}"),
                    golden_loc: golden.intent.domains[ga.0 as usize].loc.clone(),
                    revised_loc: revised.intent.domains[ra.0 as usize].loc.clone(),
                });
            }
        }
    }
}

pub(crate) fn compare_pst_crossing_matrix(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    matched_domains: &[(String, PowerDomainId, PowerDomainId)],
    g_sat: &mut PstSat,
    r_sat: &mut PstSat,
    tol_uv: i64,
    diffs: &mut Vec<UpfDiffItem>,
) {
    let g_cons = g_sat.is_state_space_consistent();
    let r_cons = r_sat.is_state_space_consistent();
    if g_cons != r_cons {
        diffs.push(UpfDiffItem {
            kind: UpfDiffKind::PstConsistencyMismatch,
            subject: "<PST>".to_string(),
            golden_detail: format!("consistent={g_cons}"),
            revised_detail: format!("consistent={r_cons}"),
            golden_loc: None,
            revised_loc: None,
        });
    } else if g_cons && r_cons {
        for (src_name, g_src, r_src) in matched_domains {
            for (dst_name, g_dst, r_dst) in matched_domains {
                if src_name == dst_name {
                    continue;
                }
                let (Some(gs_ss), Some(gd_ss), Some(rs_ss), Some(rd_ss)) = (
                    golden.primary_ss(*g_src),
                    golden.primary_ss(*g_dst),
                    revised.primary_ss(*r_src),
                    revised.primary_ss(*r_dst),
                ) else {
                    continue;
                };
                let g_off = g_sat.can_be_off_while_on(gs_ss, gd_ss).is_some();
                let r_off = r_sat.can_be_off_while_on(rs_ss, rd_ss).is_some();
                if g_off != r_off {
                    diffs.push(UpfDiffItem {
                        kind: UpfDiffKind::CrossingIsoRequirementMismatch,
                        subject: format!("{src_name} -> {dst_name}"),
                        golden_detail: format!("can_be_off_while_on={g_off}"),
                        revised_detail: format!("can_be_off_while_on={r_off}"),
                        golden_loc: None,
                        revised_loc: None,
                    });
                }
                let g_ls = g_sat.voltage_relation(gs_ss, gd_ss, tol_uv);
                let r_ls = r_sat.voltage_relation(rs_ss, rd_ss, tol_uv);
                if g_ls.kind_tag() != r_ls.kind_tag() {
                    diffs.push(UpfDiffItem {
                        kind: UpfDiffKind::CrossingLsRequirementMismatch,
                        subject: format!("{src_name} -> {dst_name}"),
                        golden_detail: format!("ls_req={}", g_ls.kind_tag()),
                        revised_detail: format!("ls_req={}", r_ls.kind_tag()),
                        golden_loc: None,
                        revised_loc: None,
                    });
                }
            }
        }
    }
}

pub(crate) fn canonical_elements(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = items
        .iter()
        .map(|e| {
            if e == "/" {
                ".".to_string()
            } else {
                escape_rtl_brackets(e.trim_start_matches('/'))
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub(crate) fn canonical_domain_name(
    resolved: &ResolvedPowerIntent,
    domain: &str,
    scope: &str,
) -> String {
    lookup_scoped(&resolved.domain_by_name, domain, scope)
        .map(|id| resolved.intent.domains[id.0 as usize].name.clone())
        .unwrap_or_else(|| domain.to_string())
}

pub(crate) fn canonical_filter_ref(
    resolved: &ResolvedPowerIntent,
    filter: &str,
    scope: &str,
) -> String {
    if let Some(pd_id) = lookup_scoped(&resolved.domain_by_name, filter, scope) {
        return format!("pd:{}", resolved.intent.domains[pd_id.0 as usize].name);
    }
    if let Some(ss_id) = resolve_ss_handle(
        filter,
        scope,
        &resolved.intent.domains,
        &resolved.domain_by_name,
        &resolved.ss_by_name,
    ) {
        let my_rep = resolved
            .equiv_class
            .get(ss_id.0 as usize)
            .copied()
            .unwrap_or(ss_id);
        let mut best_nets: Option<String> = None;
        let mut best_name: Option<String> = None;
        for (idx, ss) in resolved.intent.supply_sets.iter().enumerate() {
            let rep = resolved
                .equiv_class
                .get(idx)
                .copied()
                .unwrap_or(SupplySetId(idx as u32));
            if rep != my_rep {
                continue;
            }
            let p = resolved
                .ss_power_net
                .get(idx)
                .copied()
                .flatten()
                .map(|nid| resolved.intent.supply_nets[nid.0 as usize].name.as_str());
            let g = resolved
                .ss_ground_net
                .get(idx)
                .copied()
                .flatten()
                .map(|nid| resolved.intent.supply_nets[nid.0 as usize].name.as_str());
            if p.is_some() || g.is_some() {
                let k = format!("ss_nets:{}:{}", p.unwrap_or(""), g.unwrap_or(""));
                if best_nets.as_ref().is_none_or(|b| k < *b) {
                    best_nets = Some(k);
                }
            } else {
                let k = format!("ss:{}", ss.name);
                if best_name.as_ref().is_none_or(|b| k < *b) {
                    best_name = Some(k);
                }
            }
        }
        if let Some(k) = best_nets.or(best_name) {
            return k;
        }
        return format!("ss:{}", resolved.intent.supply_sets[ss_id.0 as usize].name);
    }
    format!("raw:{filter}")
}
