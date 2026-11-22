//! Helper functions for CPF `IllegalStateCombination` and `PstTable` maintenance
//! during CPF finalization.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{PowerIntent, PstId, PstRow, PstTable};

use super::{cpf_ss_name, CpfPowerMode};

fn parse_cpf_pst_name(name: &str) -> Option<Option<&str>> {
    if name == "cpf_power_modes" {
        Some(None)
    } else {
        name.strip_prefix("cpf_power_modes::").map(Some)
    }
}

pub(crate) fn cpf_global_illegal_target(scope: &str, design_top: Option<&str>) -> String {
    match (scope, design_top) {
        ("", None) => "__cpf_global__".to_string(),
        (sc, None) => format!("__cpf_global__@{sc}"),
        (sc, Some(d)) => format!("__cpf_global__@{sc}::{d}"),
    }
}

pub(crate) fn remove_cpf_illegal_entries(intent: &mut PowerIntent, pm: &CpfPowerMode) {
    intent.illegal_combinations.retain(|c| {
        !(c.mode_name == pm.name
            && c.scope == pm.condition_scope
            && c.source_scope == pm.scope
            && c.design_top == pm.design_top)
    });
    let global_target = cpf_global_illegal_target(&pm.scope, pm.design_top.as_deref());
    intent.power_states.retain(|s| {
        !(s.target == global_target && s.state_name == pm.name && s.scope == pm.condition_scope)
    });
}

fn find_matching_cpf_pst_table(
    pst_tables: &[PstTable],
    pm_supplies: &[String],
    scope: &str,
    design_top: Option<&str>,
) -> Option<usize> {
    pst_tables.iter().position(|t| {
        let Some(t_design) = parse_cpf_pst_name(&t.name) else {
            return false;
        };
        if t_design != design_top {
            return false;
        }
        let overlaps = pm_supplies
            .iter()
            .any(|s| t.supplies.iter().any(|ts| ts == s));
        let same_scope = t.scope == scope;
        overlaps || same_scope
    })
}

fn collect_pm_supplies(pm: &CpfPowerMode) -> Vec<String> {
    let mut pm_supplies: Vec<String> = Vec::new();
    let mut seen_sup: BTreeSet<String> = BTreeSet::new();
    for (pd, _) in &pm.domain_conditions {
        let ss = cpf_ss_name(pd);
        if seen_sup.insert(ss.clone()) {
            pm_supplies.push(ss);
        }
    }
    pm_supplies
}

fn row_matches_pm(r: &PstRow, pm: &CpfPowerMode) -> bool {
    r.state_name == pm.name && r.source_scope.as_deref() == Some(pm.scope.as_str())
}

pub(crate) fn remove_cpf_pst_row(intent: &mut PowerIntent, pm: &CpfPowerMode) {
    let design_top = pm.design_top.as_deref();
    let target_idx = intent
        .pst_tables
        .iter()
        .position(|t| {
            parse_cpf_pst_name(&t.name) == Some(design_top)
                && t.scope == pm.scope
                && t.rows.iter().any(|r| row_matches_pm(r, pm))
        })
        .or_else(|| {
            intent.pst_tables.iter().position(|t| {
                parse_cpf_pst_name(&t.name) == Some(design_top)
                    && t.rows.iter().any(|r| row_matches_pm(r, pm))
            })
        });
    let Some(idx) = target_idx else {
        return;
    };
    let t = &mut intent.pst_tables[idx];
    t.rows.retain(|r| !row_matches_pm(r, pm));
    if t.rows.is_empty() {
        intent
            .pst_tables
            .retain(|t| parse_cpf_pst_name(&t.name).is_none() || !t.rows.is_empty());
        for (i, t) in intent.pst_tables.iter_mut().enumerate() {
            t.id = PstId(i as u32);
        }
    }
}

pub(crate) fn upsert_cpf_pst_row(
    intent: &mut PowerIntent,
    pm: &CpfPowerMode,
    ss_cond_map: &BTreeMap<String, String>,
) {
    let pm_supplies = collect_pm_supplies(pm);
    if pm_supplies.is_empty() {
        return;
    }
    let design_top = pm.design_top.as_deref();
    let target_idx = intent
        .pst_tables
        .iter()
        .position(|t| {
            parse_cpf_pst_name(&t.name) == Some(design_top)
                && t.rows.iter().any(|r| row_matches_pm(r, pm))
        })
        .or_else(|| {
            find_matching_cpf_pst_table(&intent.pst_tables, &pm_supplies, &pm.scope, design_top)
        });

    if let Some(idx) = target_idx {
        let t = &mut intent.pst_tables[idx];
        for sup in &pm_supplies {
            if !t.supplies.contains(sup) {
                t.supplies.push(sup.clone());
                for existing in &mut t.rows {
                    existing.states.push("*".to_string());
                }
            }
        }
        let states: Vec<String> = t
            .supplies
            .iter()
            .map(|sup| {
                ss_cond_map
                    .get(sup)
                    .cloned()
                    .unwrap_or_else(|| "*".to_string())
            })
            .collect();
        let row = PstRow {
            state_name: pm.name.clone(),
            states,
            logic_expr: pm.condition.clone(),
            scope: pm.condition_scope.clone(),
            source_scope: Some(pm.scope.clone()),
            loc: pm.loc.clone(),
        };
        if let Some(pos) = t.rows.iter().position(|r| row_matches_pm(r, pm)) {
            t.rows[pos] = row;
        } else if pm.is_default {
            t.rows.insert(0, row);
        } else {
            t.rows.push(row);
        }
    } else {
        let states: Vec<String> = pm_supplies
            .iter()
            .map(|sup| {
                ss_cond_map
                    .get(sup)
                    .cloned()
                    .unwrap_or_else(|| "*".to_string())
            })
            .collect();
        let row = PstRow {
            state_name: pm.name.clone(),
            states,
            logic_expr: pm.condition.clone(),
            scope: pm.condition_scope.clone(),
            source_scope: Some(pm.scope.clone()),
            loc: pm.loc.clone(),
        };
        let id = PstId(intent.pst_tables.len() as u32);
        let name = match &pm.design_top {
            Some(d) => format!("cpf_power_modes::{d}"),
            None => "cpf_power_modes".to_string(),
        };
        intent.pst_tables.push(PstTable {
            id,
            name,
            scope: pm.scope.clone(),
            supplies: pm_supplies,
            rows: vec![row],
            loc: None,
        });
    }
}
