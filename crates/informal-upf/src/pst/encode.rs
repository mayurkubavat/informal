//! CNF encoding of resolved power intent, supply sets, illegal rules, and PST tables.

use std::collections::{BTreeMap, BTreeSet};

use informal_sat::{Answer, Lit};

use crate::expr_bv::{
    compile_typed_logic_ast, parse_logic_expr_in_scope, typecheck_asts, LogicConstraintAst,
    SignalWidthProvider, TypedExpr,
};
use crate::ir::{
    DiagSeverity, PowerStateDef, SimState, SupplySet, SupplySetId, UpfDiag, UpfLoc, VoltageSpec,
};
use crate::resolve::{lookup_scoped, resolve_ss_handle, ResolvedPowerIntent};

use super::cnf::CnfBuilder;
use super::supply_expr::{compile_supply_expr, extract_all_rail_voltages};
use super::{PstSat, StateEntry, SupplyLogicConstraint};

type NetPortVars = Vec<Vec<(String, VoltageSpec, bool, Lit)>>;
type SharedNetVars = Vec<Vec<(bool, VoltageSpec, Lit)>>;
type LogicParseCache = BTreeMap<(String, String), Result<(LogicConstraintAst, usize), String>>;

struct IllegalEncodeCtx<'a> {
    resolved: &'a ResolvedPowerIntent,
    net_port_vars: &'a NetPortVars,
    shared_net_vars: &'a SharedNetVars,
    parsed_logic_cache: &'a LogicParseCache,
    typed_asts: &'a [TypedExpr],
    signal_bits: &'a mut BTreeMap<String, Vec<Lit>>,
    cnf: &'a mut CnfBuilder,
    exported_constraints: &'a mut Vec<SupplyLogicConstraint>,
    diagnostics: &'a mut Vec<UpfDiag>,
}

pub(crate) fn build_pst_sat(
    resolved: &ResolvedPowerIntent,
    tol_uv: i64,
    width_provider: &dyn SignalWidthProvider,
) -> PstSat {
    let mut cnf = CnfBuilder::new();
    let mut diagnostics = Vec::new();
    let mut signal_bits: BTreeMap<String, Vec<Lit>> = BTreeMap::new();
    let mut exported_constraints = Vec::new();

    let (net_port_vars, shared_net_vars) = allocate_net_vars(resolved, &mut cnf);
    let (parsed_logic_cache, typed_asts) =
        parse_and_typecheck_logic_exprs(resolved, width_provider);

    let num_ss = resolved.intent.supply_sets.len();
    let ss_names: Vec<String> = resolved
        .intent
        .supply_sets
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let mut ss_entries: Vec<Vec<StateEntry>> = vec![Vec::new(); num_ss];
    let mut lit_normal: Vec<Lit> = vec![-cnf.const_true; num_ss];
    let mut lit_corrupt_comb: Vec<Lit> = vec![-cnf.const_true; num_ss];
    let mut unconstrained = BTreeSet::new();

    for (ssid_idx, ss) in resolved.intent.supply_sets.iter().enumerate() {
        let ssid = SupplySetId(ssid_idx as u32);
        let all_states = &resolved.ss_states[ssid_idx];
        let legal_states: Vec<&PowerStateDef> = all_states.iter().filter(|s| !s.illegal).collect();
        let illegal_states: Vec<&PowerStateDef> = all_states.iter().filter(|s| s.illegal).collect();

        if all_states.is_empty() {
            unconstrained.insert(ssid);
            diagnostics.push(UpfDiag {
                rule_id: "PST_MISSING_PORT_STATE".to_string(),
                severity: DiagSeverity::Warning,
                message: format!(
                    "supply set `{}` has no declared power states or port states; using unconstrained ON/OFF fallback",
                    ss.name
                ),
                loc: ss.loc.clone(),
            });
            let x_on = cnf.alloc_var();
            let x_off = cnf.alloc_var();
            cnf.exactly_one(&[x_on, x_off]);
            ss_entries[ssid_idx].push(StateEntry {
                state_name: "__UNCONSTRAINED_ON".to_string(),
                var: x_on,
                simstate: SimState::Normal,
                power_v: VoltageSpec::Undetermined,
                power_vs: vec![(VoltageSpec::Undetermined, None)],
                ground_vs: vec![(VoltageSpec::Nominal(0), None)],
            });
            ss_entries[ssid_idx].push(StateEntry {
                state_name: "__UNCONSTRAINED_OFF".to_string(),
                var: x_off,
                simstate: SimState::Off,
                power_v: VoltageSpec::Off,
                power_vs: vec![(VoltageSpec::Off, None)],
                ground_vs: vec![(VoltageSpec::Nominal(0), None)],
            });
            lit_normal[ssid_idx] = x_on;
            lit_corrupt_comb[ssid_idx] = x_off;
        } else if legal_states.is_empty() {
            cnf.add_clause(&[]);
        } else {
            let mut vars = Vec::with_capacity(legal_states.len());
            let mut normal_lits = Vec::new();
            let mut corrupt_comb_lits = Vec::new();
            for st in &legal_states {
                let x = cnf.alloc_var();
                vars.push(x);
                let sim = st.simstate.unwrap_or(SimState::Normal);
                let (power_vs, ground_vs) =
                    extract_all_rail_voltages(st, ss, resolved, &net_port_vars, &shared_net_vars);
                let power_v = power_vs
                    .first()
                    .map(|(v, _)| *v)
                    .unwrap_or(VoltageSpec::Undetermined);
                ss_entries[ssid_idx].push(StateEntry {
                    state_name: st.state_name.clone(),
                    var: x,
                    simstate: sim,
                    power_v,
                    power_vs,
                    ground_vs,
                });
                if sim.is_normal() {
                    normal_lits.push(x);
                }
                if sim.corrupts_combinational() {
                    corrupt_comb_lits.push(x);
                }

                // One-way implication: x_{i,j} => Phi_sup & Phi_log (never <=>)
                if let Some(sup_expr) = &st.supply_expr {
                    match compile_supply_expr(
                        sup_expr,
                        ss,
                        resolved,
                        &net_port_vars,
                        &shared_net_vars,
                        false,
                        &mut cnf,
                    ) {
                        Ok(l_sup) => cnf.add_clause(&[-x, l_sup]),
                        Err(e) => diagnostics.push(UpfDiag {
                            rule_id: "UPF_EXPR_SYNTAX".to_string(),
                            severity: DiagSeverity::Error,
                            message: format!(
                                "invalid -supply_expr `{sup_expr}` on `{}` state `{}`: {e}",
                                ss.name, st.state_name
                            ),
                            loc: st.loc.clone(),
                        }),
                    }
                }
                let mut parsed_logic_ast = None;
                if let Some(log_expr) = &st.logic_expr {
                    match parsed_logic_cache.get(&(st.scope.clone(), log_expr.clone())) {
                        Some(Ok((ast, idx))) => {
                            let l_log = compile_typed_logic_ast(
                                &typed_asts[*idx],
                                &mut signal_bits,
                                &mut cnf,
                            );
                            cnf.add_clause(&[-x, l_log]);
                            parsed_logic_ast = Some(ast.clone());
                        }
                        Some(Err(e)) => {
                            diagnostics.push(UpfDiag {
                                rule_id: "UPF_EXPR_SYNTAX".to_string(),
                                severity: DiagSeverity::Error,
                                message: format!(
                                    "invalid -logic_expr `{log_expr}` on `{}` state `{}`: {e}",
                                    ss.name, st.state_name
                                ),
                                loc: st.loc.clone(),
                            });
                        }
                        None => {}
                    }
                }
                exported_constraints.push(SupplyLogicConstraint {
                    supply_set: ss.name.clone(),
                    state_name: st.state_name.clone(),
                    is_illegal: false,
                    simstate: Some(sim),
                    logic_expr: parsed_logic_ast,
                });
            }
            cnf.exactly_one(&vars);
            lit_normal[ssid_idx] = cnf.tseitin_or(&normal_lits);
            lit_corrupt_comb[ssid_idx] = cnf.tseitin_or(&corrupt_comb_lits);
        }

        let mut ill_ctx = IllegalEncodeCtx {
            resolved,
            net_port_vars: &net_port_vars,
            shared_net_vars: &shared_net_vars,
            parsed_logic_cache: &parsed_logic_cache,
            typed_asts: &typed_asts,
            signal_bits: &mut signal_bits,
            cnf: &mut cnf,
            exported_constraints: &mut exported_constraints,
            diagnostics: &mut diagnostics,
        };
        for st in illegal_states {
            encode_illegal_rule(st, Some(ss), &mut ill_ctx);
        }
    }

    {
        let mut ill_ctx = IllegalEncodeCtx {
            resolved,
            net_port_vars: &net_port_vars,
            shared_net_vars: &shared_net_vars,
            parsed_logic_cache: &parsed_logic_cache,
            typed_asts: &typed_asts,
            signal_bits: &mut signal_bits,
            cnf: &mut cnf,
            exported_constraints: &mut exported_constraints,
            diagnostics: &mut diagnostics,
        };
        for st in &resolved.global_illegal_rules {
            encode_illegal_rule(st, None, &mut ill_ctx);
        }
        encode_illegal_combinations(&ss_entries, &mut ill_ctx);
    }

    let pst_row_selectors = encode_pst_tables(
        &net_port_vars,
        &ss_entries,
        &mut IllegalEncodeCtx {
            resolved,
            net_port_vars: &net_port_vars,
            shared_net_vars: &shared_net_vars,
            parsed_logic_cache: &parsed_logic_cache,
            typed_asts: &typed_asts,
            signal_bits: &mut signal_bits,
            cnf: &mut cnf,
            exported_constraints: &mut exported_constraints,
            diagnostics: &mut diagnostics,
        },
    );

    let consistent = cnf.sat.solve(&[]) == Answer::Sat;
    if !consistent {
        diagnostics.push(UpfDiag {
            rule_id: "PST_INCONSISTENT".to_string(),
            severity: DiagSeverity::Error,
            message: "symbolic power-state space is unsatisfiable (no legal power state exists)"
                .to_string(),
            loc: None,
        });
    } else {
        for (rho, pst_name, row_name, loc) in pst_row_selectors {
            if cnf.sat.solve(&[rho]) == Answer::Unsat {
                diagnostics.push(UpfDiag {
                    rule_id: "PST_UNREACHABLE_STATE".to_string(),
                    severity: DiagSeverity::Warning,
                    message: format!(
                        "PST `{pst_name}` row `{row_name}` is unreachable under declared constraints"
                    ),
                    loc,
                });
            }
        }
    }

    let mut solver = PstSat {
        cnf,
        ss_names,
        ss_entries,
        lit_normal,
        lit_corrupt_comb,
        unconstrained,
        equiv_class: resolved.equiv_class.clone(),
        func_equiv_class: resolved.func_equiv_class.clone(),
        signal_bits,
        exported_constraints,
        diagnostics,
        tol_uv,
        consistent,
    };

    if solver.consistent {
        solver.check_equivalence_coherence(resolved);
    }

    solver
}

fn allocate_net_vars(
    resolved: &ResolvedPowerIntent,
    cnf: &mut CnfBuilder,
) -> (NetPortVars, SharedNetVars) {
    let num_nets = resolved.intent.supply_nets.len();
    let mut net_port_vars: NetPortVars = vec![Vec::new(); num_nets];
    for (nid, sn) in resolved.intent.supply_nets.iter().enumerate() {
        if !sn.port_states.is_empty() {
            let mut vars = Vec::with_capacity(sn.port_states.len());
            for ps in &sn.port_states {
                let v = cnf.alloc_var();
                net_port_vars[nid].push((ps.state_name.clone(), ps.voltage, ps.is_off, v));
                vars.push(v);
            }
            cnf.exactly_one(&vars);
        }
    }

    let mut shared_net_outcomes: Vec<Vec<(bool, VoltageSpec)>> = vec![Vec::new(); num_nets];
    for (ssid, states) in resolved.ss_states.iter().enumerate() {
        let ss = &resolved.intent.supply_sets[ssid];
        for st in states {
            if st.illegal {
                continue;
            }
            for rail in &st.rail_states {
                if rail.voltage == VoltageSpec::Undetermined {
                    continue;
                }
                let Some(net_name) = ss.get_function(&rail.function) else {
                    continue;
                };
                let Some(nid) = lookup_scoped(&resolved.net_by_name, net_name, &ss.scope) else {
                    continue;
                };
                if !net_port_vars[nid.0 as usize].is_empty() {
                    continue;
                }
                let bucket = &mut shared_net_outcomes[nid.0 as usize];
                let key = (rail.is_off, rail.voltage);
                if !bucket.contains(&key) {
                    bucket.push(key);
                }
            }
        }
    }
    for (ssid, states) in resolved.ss_states.iter().enumerate() {
        let ss = &resolved.intent.supply_sets[ssid];
        for st in states {
            if st.illegal {
                continue;
            }
            for rail in &st.rail_states {
                if rail.voltage != VoltageSpec::Undetermined || rail.is_off {
                    continue;
                }
                let Some(net_name) = ss.get_function(&rail.function) else {
                    continue;
                };
                let Some(nid) = lookup_scoped(&resolved.net_by_name, net_name, &ss.scope) else {
                    continue;
                };
                if !net_port_vars[nid.0 as usize].is_empty() {
                    continue;
                }
                let bucket = &mut shared_net_outcomes[nid.0 as usize];
                if !bucket.iter().any(|(is_off, _)| !*is_off) {
                    bucket.push((false, VoltageSpec::Undetermined));
                }
            }
        }
    }

    let mut shared_net_vars: SharedNetVars = vec![Vec::new(); num_nets];
    for (nid, outcomes) in shared_net_outcomes.iter().enumerate() {
        if !outcomes.is_empty() {
            let mut vars = Vec::with_capacity(outcomes.len());
            for &(is_off, v) in outcomes {
                let lit = cnf.alloc_var();
                shared_net_vars[nid].push((is_off, v, lit));
                vars.push(lit);
            }
            cnf.exactly_one(&vars);
        }
    }

    (net_port_vars, shared_net_vars)
}

fn parse_and_typecheck_logic_exprs(
    resolved: &ResolvedPowerIntent,
    width_provider: &dyn SignalWidthProvider,
) -> (LogicParseCache, Vec<TypedExpr>) {
    let mut raw_cache: BTreeMap<(String, String), Result<LogicConstraintAst, String>> =
        BTreeMap::new();
    let mut register = |scope: &str, expr: &Option<String>| {
        if let Some(expr) = expr {
            raw_cache
                .entry((scope.to_string(), expr.clone()))
                .or_insert_with(|| parse_logic_expr_in_scope(expr, scope));
        }
    };

    for states in &resolved.ss_states {
        for st in states {
            register(&st.scope, &st.logic_expr);
        }
    }
    for st in &resolved.global_illegal_rules {
        register(&st.scope, &st.logic_expr);
    }
    for combo in &resolved.intent.illegal_combinations {
        register(&combo.scope, &combo.logic_expr);
        register(&combo.scope, &combo.fallback_logic_expr);
    }
    for pst in &resolved.intent.pst_tables {
        for row in &pst.rows {
            register(&row.scope, &row.logic_expr);
        }
    }

    let mut ok_keys = Vec::new();
    let mut ok_asts = Vec::new();
    for (key, res) in &raw_cache {
        if let Ok(ast) = res {
            ok_keys.push(key.clone());
            ok_asts.push(ast);
        }
    }
    let (_, typed_asts) = typecheck_asts(&ok_asts, width_provider);
    let mut idx_by_key = BTreeMap::new();
    for (idx, key) in ok_keys.into_iter().enumerate() {
        idx_by_key.insert(key, idx);
    }

    let mut cache = BTreeMap::new();
    for (key, res) in raw_cache {
        let mapped = match res {
            Ok(ast) => {
                let idx = idx_by_key[&key];
                Ok((ast, idx))
            }
            Err(e) => Err(e),
        };
        cache.insert(key, mapped);
    }

    (cache, typed_asts)
}

fn encode_illegal_combinations(ss_entries: &[Vec<StateEntry>], ctx: &mut IllegalEncodeCtx<'_>) {
    for combo in &ctx.resolved.intent.illegal_combinations {
        let mut state_lits = Vec::with_capacity(combo.domain_states.len());
        let mut all_resolved = !combo.domain_states.is_empty();
        for (ss_name, st_name) in &combo.domain_states {
            if let Some(ssid) = lookup_scoped(&ctx.resolved.ss_by_name, ss_name, &combo.scope) {
                if let Some(entry) = ss_entries[ssid.0 as usize]
                    .iter()
                    .find(|e| e.state_name == *st_name)
                {
                    state_lits.push(entry.var);
                    continue;
                }
            }
            all_resolved = false;
            break;
        }
        if all_resolved {
            let mut block: Vec<Lit> = state_lits.into_iter().map(|l| -l).collect();
            let mut parsed_ast = None;
            let mut syntax_ok = true;
            if let Some(expr) = &combo.logic_expr {
                match ctx
                    .parsed_logic_cache
                    .get(&(combo.scope.clone(), expr.clone()))
                {
                    Some(Ok((ast, idx))) => {
                        let l_log = compile_typed_logic_ast(
                            &ctx.typed_asts[*idx],
                            ctx.signal_bits,
                            ctx.cnf,
                        );
                        parsed_ast = Some(ast.clone());
                        if l_log != ctx.cnf.const_true {
                            block.push(-l_log);
                        }
                    }
                    Some(Err(e)) => {
                        ctx.diagnostics.push(UpfDiag {
                            rule_id: "UPF_EXPR_SYNTAX".to_string(),
                            severity: DiagSeverity::Error,
                            message: format!(
                                "invalid -condition `{expr}` on illegal power mode `{}`: {e}",
                                combo.mode_name
                            ),
                            loc: combo.loc.clone(),
                        });
                        syntax_ok = false;
                    }
                    None => {}
                }
            }
            if syntax_ok && !block.is_empty() {
                ctx.cnf.add_clause(&block);
                ctx.exported_constraints.push(SupplyLogicConstraint {
                    supply_set: "__cpf_global__".to_string(),
                    state_name: combo.mode_name.clone(),
                    is_illegal: true,
                    simstate: None,
                    logic_expr: parsed_ast,
                });
            }
        } else if let Some(expr) = &combo.fallback_logic_expr {
            match ctx
                .parsed_logic_cache
                .get(&(combo.scope.clone(), expr.clone()))
            {
                Some(Ok((ast, idx))) => {
                    let l_log =
                        compile_typed_logic_ast(&ctx.typed_asts[*idx], ctx.signal_bits, ctx.cnf);
                    if l_log != ctx.cnf.const_true {
                        ctx.cnf.add_clause(&[-l_log]);
                    }
                    ctx.exported_constraints.push(SupplyLogicConstraint {
                        supply_set: "__cpf_global__".to_string(),
                        state_name: combo.mode_name.clone(),
                        is_illegal: true,
                        simstate: None,
                        logic_expr: Some(ast.clone()),
                    });
                }
                Some(Err(e)) => {
                    ctx.diagnostics.push(UpfDiag {
                        rule_id: "UPF_EXPR_SYNTAX".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!(
                            "invalid condition `{expr}` on illegal power mode `{}`: {e}",
                            combo.mode_name
                        ),
                        loc: combo.loc.clone(),
                    });
                }
                None => {}
            }
        }
    }
}

fn encode_pst_tables(
    net_port_vars: &NetPortVars,
    ss_entries: &[Vec<StateEntry>],
    ctx: &mut IllegalEncodeCtx<'_>,
) -> Vec<(Lit, String, String, Option<UpfLoc>)> {
    let resolved = ctx.resolved;
    let parsed_logic_cache = ctx.parsed_logic_cache;
    let typed_asts = ctx.typed_asts;
    let signal_bits = &mut *ctx.signal_bits;
    let cnf = &mut *ctx.cnf;
    let diagnostics = &mut *ctx.diagnostics;
    let mut pst_row_selectors = Vec::new();
    for pst in &resolved.intent.pst_tables {
        if pst.rows.is_empty() {
            diagnostics.push(UpfDiag {
                rule_id: "PST_INCONSISTENT".to_string(),
                severity: DiagSeverity::Warning,
                message: format!("PST `{}` has no declared rows", pst.name),
                loc: pst.loc.clone(),
            });
            continue;
        }
        let mut valid_rhos = Vec::new();
        for row in &pst.rows {
            if row.states.len() != pst.supplies.len() {
                diagnostics.push(UpfDiag {
                    rule_id: "PST_UNREACHABLE_STATE".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "PST `{}` state `{}` has {} entries, expected {}",
                        pst.name,
                        row.state_name,
                        row.states.len(),
                        pst.supplies.len()
                    ),
                    loc: row.loc.clone(),
                });
                continue;
            }
            let mut col_lits = Vec::new();
            let mut row_valid = true;
            for (col_idx, supply_name) in pst.supplies.iter().enumerate() {
                let st_name = row.states[col_idx].trim();
                if st_name == "*" {
                    continue;
                }
                if let Some(nid) = lookup_scoped(&resolved.net_by_name, supply_name, &pst.scope) {
                    if let Some((_, _, _, lit)) = net_port_vars[nid.0 as usize]
                        .iter()
                        .find(|(nm, _, _, _)| nm == st_name)
                    {
                        col_lits.push(*lit);
                    } else {
                        diagnostics.push(UpfDiag {
                            rule_id: "PST_UNREACHABLE_STATE".to_string(),
                            severity: DiagSeverity::Error,
                            message: format!(
                                "PST `{}` row `{}` references undefined port state `{st_name}` on supply `{supply_name}`",
                                pst.name, row.state_name
                            ),
                            loc: row.loc.clone(),
                        });
                        row_valid = false;
                        break;
                    }
                } else if let Some(ssid) = resolve_ss_handle(
                    supply_name,
                    &pst.scope,
                    &resolved.intent.domains,
                    &resolved.domain_by_name,
                    &resolved.ss_by_name,
                ) {
                    if let Some(entry) = ss_entries[ssid.0 as usize]
                        .iter()
                        .find(|e| e.state_name == st_name)
                    {
                        col_lits.push(entry.var);
                    } else {
                        diagnostics.push(UpfDiag {
                            rule_id: "PST_UNREACHABLE_STATE".to_string(),
                            severity: DiagSeverity::Error,
                            message: format!(
                                "PST `{}` row `{}` references undefined supply-set state `{st_name}` on `{supply_name}`",
                                pst.name, row.state_name
                            ),
                            loc: row.loc.clone(),
                        });
                        row_valid = false;
                        break;
                    }
                } else {
                    diagnostics.push(UpfDiag {
                        rule_id: "PST_UNREACHABLE_STATE".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!(
                            "PST `{}` references unknown supply `{supply_name}`",
                            pst.name
                        ),
                        loc: pst.loc.clone(),
                    });
                    row_valid = false;
                    break;
                }
            }
            if row_valid {
                let mut row_logic_lit = None;
                if let Some(expr) = &row.logic_expr {
                    match parsed_logic_cache.get(&(row.scope.clone(), expr.clone())) {
                        Some(Ok((_, idx))) => {
                            let l_log =
                                compile_typed_logic_ast(&typed_asts[*idx], signal_bits, cnf);
                            row_logic_lit = Some(l_log);
                        }
                        Some(Err(e)) => {
                            diagnostics.push(UpfDiag {
                                rule_id: "UPF_EXPR_SYNTAX".to_string(),
                                severity: DiagSeverity::Error,
                                message: format!(
                                    "invalid -condition `{expr}` on PST `{}` row `{}`: {e}",
                                    pst.name, row.state_name
                                ),
                                loc: row.loc.clone(),
                            });
                            row_valid = false;
                        }
                        None => {}
                    }
                }
                if row_valid {
                    let rho = cnf.alloc_var();
                    for lit in col_lits {
                        cnf.add_clause(&[-rho, lit]);
                    }
                    if let Some(l_log) = row_logic_lit {
                        cnf.add_clause(&[-rho, l_log]);
                    }
                    valid_rhos.push(rho);
                    pst_row_selectors.push((
                        rho,
                        pst.name.clone(),
                        row.state_name.clone(),
                        row.loc.clone(),
                    ));
                }
            }
        }
        if valid_rhos.is_empty() {
            diagnostics.push(UpfDiag {
                rule_id: "PST_INCONSISTENT".to_string(),
                severity: DiagSeverity::Error,
                message: format!("PST `{}` has no valid reachable rows", pst.name),
                loc: pst.loc.clone(),
            });
            cnf.add_clause(&[]);
        } else {
            cnf.add_clause(&valid_rhos);
        }
    }
    pst_row_selectors
}

fn encode_illegal_rule(st: &PowerStateDef, ss: Option<&SupplySet>, ctx: &mut IllegalEncodeCtx<'_>) {
    let l_sup = match (&st.supply_expr, ss) {
        (Some(expr), Some(supply_set)) => match compile_supply_expr(
            expr,
            supply_set,
            ctx.resolved,
            ctx.net_port_vars,
            ctx.shared_net_vars,
            true,
            ctx.cnf,
        ) {
            Ok(lit) => lit,
            Err(e) => {
                ctx.diagnostics.push(UpfDiag {
                    rule_id: "UPF_EXPR_SYNTAX".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "invalid -supply_expr `{expr}` on illegal rule `{}`: {e}",
                        st.state_name
                    ),
                    loc: st.loc.clone(),
                });
                return;
            }
        },
        (Some(_), None) => {
            ctx.diagnostics.push(UpfDiag {
                rule_id: "UPF_EXPR_SYNTAX".to_string(),
                severity: DiagSeverity::Error,
                message: format!(
                    "illegal rule `{}` has -supply_expr without resolved supply set",
                    st.state_name
                ),
                loc: st.loc.clone(),
            });
            return;
        }
        (None, _) => ctx.cnf.const_true,
    };
    let mut parsed_ast = None;
    let l_log = match &st.logic_expr {
        Some(expr) => match ctx
            .parsed_logic_cache
            .get(&(st.scope.clone(), expr.clone()))
        {
            Some(Ok((ast, idx))) => {
                let lit = compile_typed_logic_ast(&ctx.typed_asts[*idx], ctx.signal_bits, ctx.cnf);
                parsed_ast = Some(ast.clone());
                lit
            }
            Some(Err(e)) => {
                ctx.diagnostics.push(UpfDiag {
                    rule_id: "UPF_EXPR_SYNTAX".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "invalid -logic_expr `{expr}` on illegal rule `{}`: {e}",
                        st.state_name
                    ),
                    loc: st.loc.clone(),
                });
                return;
            }
            None => return,
        },
        None => ctx.cnf.const_true,
    };
    let mut block = Vec::new();
    if l_sup != ctx.cnf.const_true {
        block.push(-l_sup);
    }
    if l_log != ctx.cnf.const_true {
        block.push(-l_log);
    }
    if block.is_empty() && st.supply_expr.is_none() && st.logic_expr.is_none() {
        ctx.diagnostics.push(UpfDiag {
            rule_id: "UPF_EXPR_SYNTAX".to_string(),
            severity: DiagSeverity::Warning,
            message: format!(
                "illegal rule `{}` has neither -supply_expr nor -logic_expr; ignoring",
                st.state_name
            ),
            loc: st.loc.clone(),
        });
        return;
    }
    ctx.cnf.add_clause(&block);
    ctx.exported_constraints.push(SupplyLogicConstraint {
        supply_set: ss
            .map(|s| s.name.clone())
            .unwrap_or_else(|| st.target.clone()),
        state_name: st.state_name.clone(),
        is_illegal: true,
        simstate: st.simstate,
        logic_expr: parsed_ast,
    });
}
