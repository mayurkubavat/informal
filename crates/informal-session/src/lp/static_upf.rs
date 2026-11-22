//! Stage 1 Static Power-Intent checks (`-stage upf`): UPF/CPF structural consistency,
//! supply topology, power switches, and symbolic PST satisfiability.

use std::collections::{BTreeSet, HashSet};

use informal_upf::{DiagSeverity, PstSat, ResolvedPowerIntent, UpfDiag};

use super::rules::lookup_rule;
use super::{LpSeverity, LpStatus, LpViolation};

/// Executes Stage 1 checks over `resolved` + `sat` and returns deduplicated violations.
pub fn run_stage1(resolved: &ResolvedPowerIntent, sat: &mut PstSat) -> Vec<LpViolation> {
    let mut out = Vec::new();
    let mut seen: HashSet<(String, String, Option<String>, Option<u32>)> = HashSet::new();

    // 1. Ingest diagnostics from `informal-upf` resolution and `PstSat` encoding.
    let _consistent = sat.is_state_space_consistent();
    for diag in resolved
        .intent
        .diagnostics
        .iter()
        .chain(sat.diagnostics().iter())
    {
        push_upf_diag(&mut out, &mut seen, diag);
    }

    // 2. Domain primary supply completeness (`UPF_DOMAIN_NO_SUPPLY`).
    let has_supply_decls =
        !resolved.intent.supply_nets.is_empty() || !resolved.intent.supply_sets.is_empty();
    for (idx, pd) in resolved.intent.domains.iter().enumerate() {
        let prim_ss = resolved.domain_primary_ss.get(idx).copied().flatten();
        if has_supply_decls && prim_ss.is_none() {
            push_violation(
                &mut out,
                &mut seen,
                "UPF_DOMAIN_NO_SUPPLY",
                LpSeverity::Error,
                Some(pd.name.clone()),
                None,
                pd.name.clone(),
                format!(
                    "power domain `{}` has no resolved primary supply set",
                    pd.name
                ),
                pd.loc.clone(),
            );
        } else if let Some(ss_id) = prim_ss {
            let ss = &resolved.intent.supply_sets[ss_id.0 as usize];
            if has_supply_decls && ss.power.is_none() {
                push_violation(
                    &mut out,
                    &mut seen,
                    "UPF_DOMAIN_NO_SUPPLY",
                    LpSeverity::Error,
                    Some(pd.name.clone()),
                    None,
                    ss.name.clone(),
                    format!(
                        "power domain `{}` primary supply set `{}` has no `power` rail function",
                        pd.name, ss.name
                    ),
                    pd.loc.clone().or_else(|| ss.loc.clone()),
                );
            }
        }
    }

    // 3. Dangling supply ports and partially connected supply nets (`UPF_DANGLING_NET`).
    let mut ports_in_nets: HashSet<String> = HashSet::new();
    for sn in &resolved.intent.supply_nets {
        for cp in &sn.connected_ports {
            ports_in_nets.insert(cp.trim_start_matches('/').to_string());
        }
    }
    let mut nets_in_sets: HashSet<usize> = HashSet::new();
    for (i, _) in resolved.intent.supply_sets.iter().enumerate() {
        if let Some(Some(nid)) = resolved.ss_power_net.get(i) {
            nets_in_sets.insert(nid.0 as usize);
        }
        if let Some(Some(nid)) = resolved.ss_ground_net.get(i) {
            nets_in_sets.insert(nid.0 as usize);
        }
    }
    let mut net_ids_in_switches: HashSet<usize> = HashSet::new();
    let mut nets_in_switches: HashSet<String> = HashSet::new();
    for sw in &resolved.intent.power_switches {
        for (_, net_ref) in sw
            .input_supply_ports
            .iter()
            .chain(sw.output_supply_ports.iter())
        {
            if let Some(nid) = resolved.resolve_supply_net(net_ref, &sw.scope) {
                net_ids_in_switches.insert(nid.0 as usize);
            }
            nets_in_switches.insert(net_ref.trim_start_matches('/').to_string());
        }
    }
    let mut net_ids_in_pst: HashSet<usize> = HashSet::new();
    for pst in &resolved.intent.pst_tables {
        for s in &pst.supplies {
            if let Some(nid) = resolved.resolve_supply_net(s, &pst.scope) {
                net_ids_in_pst.insert(nid.0 as usize);
            }
        }
    }

    for sp in &resolved.intent.supply_ports {
        let leaf = sp.short_name.as_str();
        let used_in_net = ports_in_nets.contains(&sp.name) || ports_in_nets.contains(leaf);
        let used_in_sw = nets_in_switches.contains(&sp.name) || nets_in_switches.contains(leaf);
        if !used_in_net && !used_in_sw {
            push_violation(
                &mut out,
                &mut seen,
                "UPF_DANGLING_NET",
                LpSeverity::Warning,
                canonical_domain_opt(resolved, sp.domain.as_deref(), &sp.scope),
                None,
                sp.name.clone(),
                format!(
                    "supply port `{}` is declared but not connected to any supply net or switch",
                    sp.name
                ),
                sp.loc.clone(),
            );
        }
    }

    for (idx, sn) in resolved.intent.supply_nets.iter().enumerate() {
        let used = nets_in_sets.contains(&idx)
            || net_ids_in_switches.contains(&idx)
            || net_ids_in_pst.contains(&idx);
        if !used && !sn.connected_ports.is_empty() && sn.port_states.is_empty() {
            push_violation(
                &mut out,
                &mut seen,
                "UPF_DANGLING_NET",
                LpSeverity::Warning,
                canonical_domain_opt(resolved, sn.domain.as_deref(), &sn.scope),
                None,
                sn.name.clone(),
                format!(
                    "supply net `{}` is connected to port(s) but unused by any supply set, switch, or PST",
                    sn.name
                ),
                sn.loc.clone(),
            );
        }
    }

    // 4. Power switch input/output supply connectivity (`PSW_SUPPLY_UNCONNECTED`).
    for sw in &resolved.intent.power_switches {
        let sw_dom = canonical_domain_opt(resolved, Some(sw.domain.as_str()), &sw.scope);
        if sw.input_supply_ports.is_empty() || sw.output_supply_ports.is_empty() {
            push_violation(
                &mut out,
                &mut seen,
                "PSW_SUPPLY_UNCONNECTED",
                LpSeverity::Error,
                sw_dom,
                None,
                sw.name.clone(),
                format!(
                    "power switch `{}` is missing input or output supply port connection",
                    sw.name
                ),
                sw.loc.clone(),
            );
            continue;
        }
        for (port_n, net_ref) in sw
            .input_supply_ports
            .iter()
            .chain(sw.output_supply_ports.iter())
        {
            if resolved.resolve_supply_net(net_ref, &sw.scope).is_none() {
                push_violation(
                    &mut out,
                    &mut seen,
                    "PSW_SUPPLY_UNCONNECTED",
                    LpSeverity::Error,
                    sw_dom.clone(),
                    None,
                    format!("{}/{port_n}", sw.name),
                    format!(
                        "power switch `{}` port `{port_n}` references unresolved supply net `{net_ref}`",
                        sw.name
                    ),
                    sw.loc.clone(),
                );
            }
        }
    }

    // 5. Isolation clamp specification check in Stage 1 (`ISO_CLAMP_UNSPECIFIED`).
    for iso in &resolved.intent.isolation_strategies {
        if !iso.no_isolation && iso.clamp_value.is_none() && iso.applies_to_clamp.is_none() {
            let iso_dom = canonical_domain_opt(resolved, Some(iso.domain.as_str()), &iso.scope);
            let dom_lbl = iso_dom.as_deref().unwrap_or(&iso.domain);
            push_violation(
                &mut out,
                &mut seen,
                "ISO_CLAMP_UNSPECIFIED",
                LpSeverity::Warning,
                iso_dom.clone(),
                None,
                iso.name.clone(),
                format!(
                    "isolation strategy `{}` on domain `{dom_lbl}` has no `-clamp_value`",
                    iso.name
                ),
                iso.loc.clone(),
            );
        }
    }

    // 6. Retention `set_retention_elements` empty-list check (`RET_NO_ELEMENTS`) and missing PST port states.
    for op in &resolved.intent.deferred_ops {
        if let informal_upf::DeferredOp::SetRetentionElements {
            name,
            scope,
            domain,
            elements,
            loc,
            ..
        } = op
        {
            if elements.is_empty() {
                let dom_opt = canonical_domain_opt(resolved, Some(domain.as_str()), scope);
                let dom_lbl = dom_opt.as_deref().unwrap_or(domain);
                push_violation(
                    &mut out,
                    &mut seen,
                    "RET_NO_ELEMENTS",
                    LpSeverity::Warning,
                    dom_opt.clone(),
                    None,
                    name.clone(),
                    format!("set_retention_elements `{name}` on domain `{dom_lbl}` specifies empty `-elements`"),
                    loc.clone(),
                );
            }
        }
    }

    // Also ensure any supply net in a PST without port_states triggers `PST_MISSING_PORT_STATE`.
    let mut pst_warned = BTreeSet::new();
    for pst in &resolved.intent.pst_tables {
        for sup_name in &pst.supplies {
            if let Some(nid) = resolved.resolve_supply_net(sup_name, &pst.scope) {
                let sn = &resolved.intent.supply_nets[nid.0 as usize];
                if sn.port_states.is_empty() && pst_warned.insert(sn.name.clone()) {
                    push_violation(
                        &mut out,
                        &mut seen,
                        "PST_MISSING_PORT_STATE",
                        LpSeverity::Warning,
                        canonical_domain_opt(resolved, sn.domain.as_deref(), &sn.scope),
                        None,
                        sn.name.clone(),
                        format!(
                            "supply net `{}` referenced in PST `{}` has no `add_port_state` declarations",
                            sn.name, pst.name
                        ),
                        pst.loc.clone().or_else(|| sn.loc.clone()),
                    );
                }
            }
        }
    }

    out
}

fn canonical_domain_opt(
    resolved: &ResolvedPowerIntent,
    raw: Option<&str>,
    scope: &str,
) -> Option<String> {
    let raw = raw?;
    if raw.is_empty() {
        return Some(String::new());
    }
    Some(
        resolved
            .resolve_domain(raw, scope)
            .and_then(|id| resolved.domain_name(id))
            .unwrap_or(raw)
            .to_string(),
    )
}

fn push_upf_diag(
    out: &mut Vec<LpViolation>,
    seen: &mut HashSet<(String, String, Option<String>, Option<u32>)>,
    diag: &UpfDiag,
) {
    let severity = match diag.severity {
        DiagSeverity::Error => LpSeverity::Error,
        DiagSeverity::Warning => LpSeverity::Warning,
        DiagSeverity::Note => LpSeverity::Note,
    };
    let signal = extract_backtick_token(&diag.message).unwrap_or_else(|| diag.rule_id.clone());
    push_violation(
        out,
        seen,
        &diag.rule_id,
        severity,
        None,
        None,
        signal,
        diag.message.clone(),
        diag.loc.clone(),
    );
}

#[allow(clippy::too_many_arguments)]
fn push_violation(
    out: &mut Vec<LpViolation>,
    seen: &mut HashSet<(String, String, Option<String>, Option<u32>)>,
    rule_id: &str,
    severity: LpSeverity,
    domain_src: Option<String>,
    domain_dst: Option<String>,
    signal: String,
    message: String,
    upf_loc: Option<informal_upf::UpfLoc>,
) {
    let key = (
        rule_id.to_string(),
        message.clone(),
        upf_loc.as_ref().map(|l| l.file.clone()),
        upf_loc.as_ref().map(|l| l.line),
    );
    if !seen.insert(key) {
        return;
    }
    let stage = lookup_rule(rule_id)
        .map(|r| r.stage.word().to_string())
        .unwrap_or_else(|| "upf".to_string());
    out.push(LpViolation {
        id: out.len() + 1,
        rule_id: rule_id.to_string(),
        severity,
        stage,
        domain_src,
        domain_dst,
        signal,
        width_bits: 1,
        raw_crossings: 1,
        message,
        upf_loc,
        rtl_loc: None,
        pst_witness: None,
        status: LpStatus::Open,
        waiver_id: None,
        formal_prop: None,
        trace: None,
        auto_formal_meta: None,
    });
}

fn extract_backtick_token(msg: &str) -> Option<String> {
    let start = msg.find('`')?;
    let rest = &msg[start + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}
