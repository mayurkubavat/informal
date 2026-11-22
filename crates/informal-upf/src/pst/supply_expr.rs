//! UPF `-supply_expr` parser, CNF compiler, and rail voltage extraction.

use informal_sat::Lit;

use crate::expr_bv::lexer::{tokenize_expr, ExprTok};
use crate::ir::{strip_outer_braces, PowerStateDef, SupplySet, VoltageSpec};
use crate::resolve::{lookup_scoped, ResolvedPowerIntent};

use super::cnf::CnfBuilder;

pub(crate) type RailVoltageChoices = (
    Vec<(VoltageSpec, Option<Lit>)>,
    Vec<(VoltageSpec, Option<Lit>)>,
);

struct SupExprCtx<'a> {
    ss: &'a SupplySet,
    resolved: &'a ResolvedPowerIntent,
    net_port_vars: &'a [Vec<(String, VoltageSpec, bool, Lit)>],
    shared_net_vars: &'a [Vec<(bool, VoltageSpec, Lit)>],
    is_illegal: bool,
}

pub(crate) fn compile_supply_expr(
    expr: &str,
    ss: &SupplySet,
    resolved: &ResolvedPowerIntent,
    net_port_vars: &[Vec<(String, VoltageSpec, bool, Lit)>],
    shared_net_vars: &[Vec<(bool, VoltageSpec, Lit)>],
    is_illegal: bool,
    cnf: &mut CnfBuilder,
) -> Result<Lit, String> {
    let tokens = tokenize_expr(expr)?;
    if tokens.is_empty() {
        return Err("empty -supply_expr".to_string());
    }
    let ctx = SupExprCtx {
        ss,
        resolved,
        net_port_vars,
        shared_net_vars,
        is_illegal,
    };
    let mut pos = 0;
    let lit = parse_sup_or(&tokens, &mut pos, &ctx, cnf)?;
    if pos != tokens.len() {
        return Err(format!(
            "unexpected trailing token {:?} in -supply_expr",
            tokens[pos]
        ));
    }
    Ok(lit)
}

fn parse_sup_or(
    tokens: &[ExprTok],
    pos: &mut usize,
    ctx: &SupExprCtx<'_>,
    cnf: &mut CnfBuilder,
) -> Result<Lit, String> {
    let mut lits = vec![parse_sup_and(tokens, pos, ctx, cnf)?];
    while matches!(tokens.get(*pos), Some(ExprTok::OrOr)) {
        *pos += 1;
        lits.push(parse_sup_and(tokens, pos, ctx, cnf)?);
    }
    Ok(cnf.tseitin_or(&lits))
}

fn parse_sup_and(
    tokens: &[ExprTok],
    pos: &mut usize,
    ctx: &SupExprCtx<'_>,
    cnf: &mut CnfBuilder,
) -> Result<Lit, String> {
    let mut lits = vec![parse_sup_atom(tokens, pos, ctx, cnf)?];
    while matches!(tokens.get(*pos), Some(ExprTok::AndAnd)) {
        *pos += 1;
        lits.push(parse_sup_atom(tokens, pos, ctx, cnf)?);
    }
    Ok(cnf.tseitin_and(&lits))
}

fn parse_sup_atom(
    tokens: &[ExprTok],
    pos: &mut usize,
    ctx: &SupExprCtx<'_>,
    cnf: &mut CnfBuilder,
) -> Result<Lit, String> {
    if matches!(tokens.get(*pos), Some(ExprTok::Bang)) {
        *pos += 1;
        return Ok(-parse_sup_atom(tokens, pos, ctx, cnf)?);
    }
    if matches!(tokens.get(*pos), Some(ExprTok::LParen)) {
        *pos += 1;
        let inner = parse_sup_or(tokens, pos, ctx, cnf)?;
        if !matches!(tokens.get(*pos), Some(ExprTok::RParen)) {
            return Err("expected `)` in -supply_expr".to_string());
        }
        *pos += 1;
        return Ok(inner);
    }
    let lhs = match tokens.get(*pos) {
        Some(ExprTok::Ident(tok)) => {
            *pos += 1;
            tok.as_path_string()
        }
        other => {
            return Err(format!(
                "expected rail/net identifier in -supply_expr, got {other:?}"
            ))
        }
    };
    let is_eq = match tokens.get(*pos) {
        Some(ExprTok::EqEq) => {
            *pos += 1;
            true
        }
        Some(ExprTok::BangEq) => {
            *pos += 1;
            false
        }
        other => {
            return Err(format!(
                "expected `==` or `!=` in -supply_expr, got {other:?}"
            ))
        }
    };
    let rhs_raw = match tokens.get(*pos) {
        Some(ExprTok::BraceGroup(s)) | Some(ExprTok::Number(s)) => {
            *pos += 1;
            s.clone()
        }
        Some(ExprTok::Ident(tok)) => {
            *pos += 1;
            tok.as_path_string()
        }
        other => {
            return Err(format!(
                "expected voltage/state value in -supply_expr, got {other:?}"
            ))
        }
    };

    let nid_opt = ctx
        .ss
        .get_function(&lhs)
        .and_then(|nm| lookup_scoped(&ctx.resolved.net_by_name, nm, &ctx.ss.scope))
        .or_else(|| lookup_scoped(&ctx.resolved.net_by_name, &lhs, &ctx.ss.scope));
    let Some(nid) = nid_opt else {
        return if ctx.is_illegal {
            Err(format!(
                "unresolved supply rail/net `{lhs}` in -supply_expr"
            ))
        } else {
            let v = cnf.alloc_var();
            Ok(if is_eq { v } else { -v })
        };
    };

    let port_vars = &ctx.net_port_vars[nid.0 as usize];
    if !port_vars.is_empty() {
        let stripped = strip_outer_braces(&rhs_raw);
        let parsed_v = VoltageSpec::parse(stripped).ok();
        let matching: Vec<Lit> = port_vars
            .iter()
            .filter(|(ps_name, ps_v, _, _)| {
                ps_name == stripped || parsed_v.is_some_and(|v| &v == ps_v)
            })
            .map(|(_, _, _, lit)| *lit)
            .collect();
        let base = if matching.is_empty() {
            -cnf.const_true
        } else {
            cnf.tseitin_or(&matching)
        };
        return Ok(if is_eq { base } else { -base });
    }

    let shared_vars = &ctx.shared_net_vars[nid.0 as usize];
    if !shared_vars.is_empty() {
        let v = VoltageSpec::parse(&rhs_raw)?;
        let key_off = v.is_off();
        let matching: Vec<Lit> = shared_vars
            .iter()
            .filter(|(is_off, sv, _)| *is_off == key_off && *sv == v)
            .map(|(_, _, lit)| *lit)
            .collect();
        let base = if matching.is_empty() {
            -cnf.const_true
        } else {
            cnf.tseitin_or(&matching)
        };
        return Ok(if is_eq { base } else { -base });
    }

    if ctx.is_illegal {
        Err(format!(
            "supply rail `{lhs}` has no declared states in -illegal -supply_expr"
        ))
    } else {
        let v = cnf.alloc_var();
        Ok(if is_eq { v } else { -v })
    }
}

pub(crate) fn find_rail_lit(
    ss: &SupplySet,
    func: &str,
    v: VoltageSpec,
    resolved: &ResolvedPowerIntent,
    net_port_vars: &[Vec<(String, VoltageSpec, bool, Lit)>],
    shared_net_vars: &[Vec<(bool, VoltageSpec, Lit)>],
) -> Option<Lit> {
    let net_name = ss.get_function(func)?;
    let nid = lookup_scoped(&resolved.net_by_name, net_name, &ss.scope)?;
    let port_vars = &net_port_vars[nid.0 as usize];
    if !port_vars.is_empty() {
        let matches: Vec<Lit> = port_vars
            .iter()
            .filter(|(_, pv, _, _)| *pv == v)
            .map(|(_, _, _, lit)| *lit)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        return None;
    }
    let shared = &shared_net_vars[nid.0 as usize];
    shared
        .iter()
        .find(|(is_off, sv, _)| *is_off == v.is_off() && *sv == v)
        .map(|(_, _, lit)| *lit)
}

pub(crate) fn extract_all_rail_voltages(
    st: &PowerStateDef,
    ss: &SupplySet,
    resolved: &ResolvedPowerIntent,
    net_port_vars: &[Vec<(String, VoltageSpec, bool, Lit)>],
    shared_net_vars: &[Vec<(bool, VoltageSpec, Lit)>],
) -> RailVoltageChoices {
    let mut pwr_vs: Vec<(VoltageSpec, Option<Lit>)> = Vec::new();
    let mut gnd_vs: Vec<(VoltageSpec, Option<Lit>)> = Vec::new();
    for r in &st.rail_states {
        if r.function == "power" {
            if !pwr_vs.iter().any(|(v, _)| *v == r.voltage) {
                let lit = find_rail_lit(
                    ss,
                    "power",
                    r.voltage,
                    resolved,
                    net_port_vars,
                    shared_net_vars,
                );
                pwr_vs.push((r.voltage, lit));
            }
        } else if r.function == "ground" && !gnd_vs.iter().any(|(v, _)| *v == r.voltage) {
            let lit = find_rail_lit(
                ss,
                "ground",
                r.voltage,
                resolved,
                net_port_vars,
                shared_net_vars,
            );
            gnd_vs.push((r.voltage, lit));
        }
    }
    if pwr_vs.is_empty() {
        let fallback = if st.simstate.is_some_and(|s| !s.is_normal()) {
            VoltageSpec::Off
        } else {
            VoltageSpec::Undetermined
        };
        pwr_vs.push((fallback, None));
    }
    if gnd_vs.is_empty() {
        gnd_vs.push((VoltageSpec::Nominal(0), None));
    }
    (pwr_vs, gnd_vs)
}
