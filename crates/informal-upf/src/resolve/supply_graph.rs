//! Supply port/net resolution, domain supply materialization, cycle DFS, and dangling checks.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{
    last_hier_segment, normalize_scope, split_handle_dot, split_parent_leaf, trim_hier_path,
    DeferredOp, DiagSeverity, PowerDomain, PowerDomainId, PowerIntent, SupplyNet, SupplyNetId,
    SupplyPortId, SupplySet, SupplySetId, UpfDiag, UpfLoc,
};

use super::ResolvedPowerIntent;

pub(crate) fn resolve_supply_graph(
    work: &mut PowerIntent,
    ops: &[DeferredOp],
) -> (
    BTreeMap<String, SupplyPortId>,
    BTreeMap<String, SupplyNetId>,
) {
    let port_by_name: BTreeMap<String, SupplyPortId> = work
        .supply_ports
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), SupplyPortId(i as u32)))
        .collect();
    let mut net_by_name: BTreeMap<String, SupplyNetId> = BTreeMap::new();
    for (i, n) in work.supply_nets.iter().enumerate() {
        net_by_name
            .entry(n.name.clone())
            .or_insert(SupplyNetId(i as u32));
    }

    for op in ops {
        match op {
            DeferredOp::ConnectSupplyNet {
                net,
                scope,
                ports,
                pins,
                domain,
                loc,
                ..
            } => {
                if let Some(nid) = lookup_scoped(&net_by_name, net, scope) {
                    let sn = &mut work.supply_nets[nid.0 as usize];
                    for p in ports {
                        if !sn.connected_ports.contains(p) {
                            sn.connected_ports.push(p.clone());
                        }
                    }
                    for pin in pins {
                        if !sn.connected_pins.contains(pin) {
                            sn.connected_pins.push(pin.clone());
                        }
                    }
                    if sn.domain.is_none() {
                        sn.domain = domain.clone();
                    }
                } else {
                    work.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!("connect_supply_net: undeclared supply net `{net}`"),
                        loc: loc.clone(),
                    });
                }
            }
            DeferredOp::GlobalConnection {
                net,
                pins,
                scope,
                loc,
                ..
            } => {
                if let Some(nid) = lookup_scoped(&net_by_name, net, scope) {
                    let sn = &mut work.supply_nets[nid.0 as usize];
                    for pin in pins {
                        if !sn.connected_pins.contains(pin) {
                            sn.connected_pins.push(pin.clone());
                        }
                    }
                } else {
                    work.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Warning,
                        message: format!("create_global_connection: undeclared supply net `{net}`"),
                        loc: loc.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    let mut port_to_net: BTreeMap<String, SupplyNetId> = BTreeMap::new();
    for (nid_idx, sn) in work.supply_nets.iter().enumerate() {
        let nid = SupplyNetId(nid_idx as u32);
        for p in &sn.connected_ports {
            port_to_net.entry(p.clone()).or_insert(nid);
        }
    }
    for sw in &mut work.power_switches {
        let sw_scope = normalize_scope(&sw.scope, '/');
        let sw_name = normalize_scope(&sw.name, '/');
        let sw_short = last_hier_segment(&sw_name);
        let mut register_or_backfill = |ports: &mut Vec<(String, String)>| {
            for (port_name, net_name) in ports.iter_mut() {
                if net_name.is_empty() {
                    let p1 = format!("{sw_name}/{port_name}");
                    let p2 = format!("{sw_scope}/{sw_short}/{port_name}");
                    let found = lookup_scoped(&port_to_net, &p1, &sw_scope).or_else(|| {
                        if !sw_scope.is_empty() && !sw_name.starts_with(&format!("{sw_scope}/")) {
                            lookup_scoped(&port_to_net, &p2, &sw_scope)
                        } else {
                            None
                        }
                    });
                    if let Some(nid) = found {
                        *net_name = format!(
                            "/{}",
                            work.supply_nets[nid.0 as usize]
                                .name
                                .trim_start_matches('/')
                        );
                    }
                }
                if !net_name.is_empty() {
                    if let Some(nid) = lookup_scoped(&net_by_name, net_name, &sw_scope) {
                        port_to_net
                            .entry(format!("{sw_name}/{port_name}"))
                            .or_insert(nid);
                        if !sw_scope.is_empty() && !sw_name.starts_with(&format!("{sw_scope}/")) {
                            port_to_net
                                .entry(format!("{sw_scope}/{sw_short}/{port_name}"))
                                .or_insert(nid);
                        }
                    }
                }
            }
        };
        register_or_backfill(&mut sw.output_supply_ports);
        register_or_backfill(&mut sw.input_supply_ports);
    }
    for op in ops {
        if let DeferredOp::AddPortState {
            target,
            scope,
            states,
            loc,
        } = op
        {
            if let Some(nid) = lookup_scoped(&net_by_name, target, scope)
                .or_else(|| lookup_scoped(&port_to_net, target, scope))
            {
                let existing = &mut work.supply_nets[nid.0 as usize].port_states;
                for s in states {
                    if !existing.iter().any(|p| {
                        p.state_name == s.state_name
                            && p.voltage == s.voltage
                            && p.is_off == s.is_off
                    }) {
                        existing.push(s.clone());
                    }
                }
            } else {
                let norm_target = target.trim_start_matches('/').to_string();
                let id = SupplyNetId(work.supply_nets.len() as u32);
                work.supply_nets.push(SupplyNet {
                    id,
                    name: norm_target.clone(),
                    short_name: last_hier_segment(&norm_target).to_string(),
                    scope: scope.clone(),
                    domain: None,
                    resolve: None,
                    reuse: true,
                    connected_ports: vec![norm_target.clone()],
                    connected_pins: Vec::new(),
                    port_states: states.clone(),
                    nominal_voltage: None,
                    is_ground: false,
                    is_internal: false,
                    loc: loc.clone(),
                });
                net_by_name.insert(norm_target.clone(), id);
                port_to_net.insert(norm_target, id);
            }
        }
    }
    for pst in &mut work.pst_tables {
        let pst_scope = pst.scope.clone();
        for sup in &mut pst.supplies {
            let sup_key = sup.trim_start_matches('/');
            let is_known_ss = if sup.starts_with('/') {
                work.supply_sets.iter().any(|ss| ss.name == sup_key)
            } else {
                work.supply_sets.iter().any(|ss| ss.name == *sup)
            };
            if !is_known_ss && lookup_scoped(&net_by_name, sup, &pst_scope).is_none() {
                if let Some(nid) = lookup_scoped(&port_to_net, sup, &pst_scope) {
                    *sup = format!(
                        "/{}",
                        work.supply_nets[nid.0 as usize]
                            .name
                            .trim_start_matches('/')
                    );
                }
            }
        }
    }

    detect_supply_cycles(work, &net_by_name);
    (port_by_name, net_by_name)
}

pub(crate) fn materialize_domain_supplies(
    work: &mut PowerIntent,
    ops: &[DeferredOp],
    net_by_name: &BTreeMap<String, SupplyNetId>,
) -> (
    BTreeMap<String, PowerDomainId>,
    BTreeMap<String, SupplySetId>,
) {
    let mut domain_by_name: BTreeMap<String, PowerDomainId> = BTreeMap::new();
    for (i, d) in work.domains.iter_mut().enumerate() {
        d.id = PowerDomainId(i as u32);
        domain_by_name.insert(d.name.clone(), d.id);
    }

    let mut ss_by_name: BTreeMap<String, SupplySetId> = BTreeMap::new();
    for (i, s) in work.supply_sets.iter_mut().enumerate() {
        s.id = SupplySetId(i as u32);
        ss_by_name.insert(s.name.clone(), s.id);
    }

    for op in ops {
        if let DeferredOp::SetDomainSupplyNet {
            domain,
            scope,
            primary_power_net,
            primary_ground_net,
            loc,
        } = op
        {
            if let Some(pd_id) = lookup_scoped(&domain_by_name, domain, scope) {
                let pd_name = work.domains[pd_id.0 as usize].name.clone();
                let pd_scope = work.domains[pd_id.0 as usize].scope.clone();
                let ss_name = work.domains[pd_id.0 as usize]
                    .primary_supply_set
                    .clone()
                    .unwrap_or_else(|| format!("{pd_name}.primary"));
                let ssid = ensure_supply_set(
                    work,
                    &mut ss_by_name,
                    &ss_name,
                    &pd_scope,
                    true,
                    loc.clone(),
                );
                let ss = &mut work.supply_sets[ssid.0 as usize];
                if let Some(p) = primary_power_net {
                    ss.power = Some(p.clone());
                }
                if let Some(g) = primary_ground_net {
                    ss.ground = Some(g.clone());
                }
                let pd = &mut work.domains[pd_id.0 as usize];
                pd.primary_supply_set = Some(ss_name.clone());
                pd.supply_handles.insert("primary".to_string(), ss_name);
                if let Some(p) = primary_power_net {
                    pd.legacy_primary_power_net = Some(p.clone());
                }
                if let Some(g) = primary_ground_net {
                    pd.legacy_primary_ground_net = Some(g.clone());
                }
            } else {
                work.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!("set_domain_supply_net: unknown power domain `{domain}`"),
                    loc: loc.clone(),
                });
            }
        }
    }

    for op in ops {
        if let DeferredOp::AssociateSupplySet {
            supply_set,
            handle,
            scope,
            loc,
        } = op
        {
            let mut resolved_ss = resolve_ss_handle(
                supply_set,
                scope,
                &work.domains,
                &domain_by_name,
                &ss_by_name,
            );
            if resolved_ss.is_none() && work.create_implicit_supply_sets {
                if let Some((pd_raw, h_name)) = split_handle_dot(supply_set) {
                    if h_name == "primary" {
                        if let Some(pd_id) = lookup_scoped(&domain_by_name, pd_raw, scope) {
                            let pd_name = work.domains[pd_id.0 as usize].name.clone();
                            let pd_scope = work.domains[pd_id.0 as usize].scope.clone();
                            let pd_loc = work.domains[pd_id.0 as usize].loc.clone();
                            let implicit_name = format!("{pd_name}.primary");
                            let ssid = ensure_supply_set(
                                work,
                                &mut ss_by_name,
                                &implicit_name,
                                &pd_scope,
                                true,
                                pd_loc,
                            );
                            work.domains[pd_id.0 as usize].primary_supply_set =
                                Some(implicit_name.clone());
                            work.domains[pd_id.0 as usize]
                                .supply_handles
                                .entry("primary".to_string())
                                .or_insert(implicit_name);
                            resolved_ss = Some(ssid);
                        }
                    }
                }
            }
            let ss_canonical = match resolved_ss {
                Some(id) => work.supply_sets[id.0 as usize].name.clone(),
                None => {
                    work.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!("associate_supply_set: unknown supply set `{supply_set}`"),
                        loc: loc.clone(),
                    });
                    continue;
                }
            };
            if let Some((pd_raw, h_name)) = split_handle_dot(handle) {
                if let Some(pd_id) = lookup_scoped(&domain_by_name, pd_raw, scope) {
                    let pd = &mut work.domains[pd_id.0 as usize];
                    pd.supply_handles
                        .insert(h_name.to_string(), ss_canonical.clone());
                    if h_name == "primary" {
                        pd.primary_supply_set = Some(ss_canonical.clone());
                    }
                } else {
                    work.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!(
                            "associate_supply_set: unknown domain in handle `{handle}`"
                        ),
                        loc: loc.clone(),
                    });
                }
            } else {
                work.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "associate_supply_set: invalid handle `{handle}` (expected PD.handle)"
                    ),
                    loc: loc.clone(),
                });
            }
        }
    }

    let domain_count = work.domains.len();
    for idx in 0..domain_count {
        if work.domains[idx].primary_supply_set.is_none() && work.create_implicit_supply_sets {
            let pd_name = work.domains[idx].name.clone();
            let pd_scope = work.domains[idx].scope.clone();
            let pd_loc = work.domains[idx].loc.clone();
            let implicit_name = format!("{pd_name}.primary");
            ensure_supply_set(
                work,
                &mut ss_by_name,
                &implicit_name,
                &pd_scope,
                true,
                pd_loc,
            );
            work.domains[idx].primary_supply_set = Some(implicit_name.clone());
            work.domains[idx]
                .supply_handles
                .entry("primary".to_string())
                .or_insert(implicit_name);
        }
    }

    for idx in 0..work.isolation_strategies.len() {
        if work.isolation_strategies[idx]
            .isolation_supply_set
            .is_none()
            && (work.isolation_strategies[idx].isolation_power_net.is_some()
                || work.isolation_strategies[idx]
                    .isolation_ground_net
                    .is_some())
        {
            let ss_name = format!(
                "__iso_ss_{}_{}",
                work.isolation_strategies[idx].domain, work.isolation_strategies[idx].name
            );
            let sc = work.isolation_strategies[idx].scope.clone();
            let loc = work.isolation_strategies[idx].loc.clone();
            let ssid = ensure_supply_set(work, &mut ss_by_name, &ss_name, &sc, true, loc);
            let pwr = work.isolation_strategies[idx].isolation_power_net.clone();
            let gnd = work.isolation_strategies[idx].isolation_ground_net.clone();
            let ss = &mut work.supply_sets[ssid.0 as usize];
            if ss.power.is_none() {
                ss.power = pwr;
            }
            if ss.ground.is_none() {
                ss.ground = gnd;
            }
            work.isolation_strategies[idx].isolation_supply_set = Some(ss_name);
        }
    }
    for idx in 0..work.retention_strategies.len() {
        if work.retention_strategies[idx]
            .retention_supply_set
            .is_none()
            && (work.retention_strategies[idx].retention_power_net.is_some()
                || work.retention_strategies[idx]
                    .retention_ground_net
                    .is_some())
        {
            let ss_name = format!(
                "__ret_ss_{}_{}",
                work.retention_strategies[idx].domain, work.retention_strategies[idx].name
            );
            let sc = work.retention_strategies[idx].scope.clone();
            let loc = work.retention_strategies[idx].loc.clone();
            let ssid = ensure_supply_set(work, &mut ss_by_name, &ss_name, &sc, true, loc);
            let pwr = work.retention_strategies[idx].retention_power_net.clone();
            let gnd = work.retention_strategies[idx].retention_ground_net.clone();
            let ss = &mut work.supply_sets[ssid.0 as usize];
            if ss.power.is_none() {
                ss.power = pwr;
            }
            if ss.ground.is_none() {
                ss.ground = gnd;
            }
            work.retention_strategies[idx].retention_supply_set = Some(ss_name);
        }
    }

    check_dangling_supply_nets(work, net_by_name);
    (domain_by_name, ss_by_name)
}

type RailNetVecs = (
    Vec<Option<SupplySetId>>,
    Vec<Option<SupplyNetId>>,
    Vec<Option<SupplyNetId>>,
);

pub(crate) fn resolve_ss_rail_nets(
    work: &PowerIntent,
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    ss_by_name: &BTreeMap<String, SupplySetId>,
    net_by_name: &BTreeMap<String, SupplyNetId>,
) -> RailNetVecs {
    let domain_primary_ss: Vec<Option<SupplySetId>> = work
        .domains
        .iter()
        .map(|pd| {
            pd.primary_supply_set.as_deref().and_then(|s| {
                resolve_ss_handle(s, &pd.scope, &work.domains, domain_by_name, ss_by_name)
            })
        })
        .collect();

    let ss_power_net: Vec<Option<SupplyNetId>> = work
        .supply_sets
        .iter()
        .map(|ss| {
            ss.power
                .as_deref()
                .and_then(|n| lookup_scoped(net_by_name, n, &ss.scope))
        })
        .collect();
    let ss_ground_net: Vec<Option<SupplyNetId>> = work
        .supply_sets
        .iter()
        .map(|ss| {
            ss.ground
                .as_deref()
                .and_then(|n| lookup_scoped(net_by_name, n, &ss.scope))
        })
        .collect();

    (domain_primary_ss, ss_power_net, ss_ground_net)
}

pub(crate) fn lookup_scoped<V: Copy>(
    map: &BTreeMap<String, V>,
    raw: &str,
    scope: &str,
) -> Option<V> {
    let trimmed = trim_hier_path(raw);
    let is_abs = trimmed.starts_with('/');
    let key = trimmed.trim_start_matches('/');
    if is_abs || scope.is_empty() {
        if let Some(&v) = map.get(key) {
            return Some(v);
        }
    }
    if !scope.is_empty() && !is_abs {
        let local_key = key.strip_prefix(&format!("{scope}/")).unwrap_or(key);
        let scoped = format!("{scope}/{local_key}");
        if let Some(&v) = map.get(&scoped) {
            return Some(v);
        }
        if local_key == key {
            if let Some(&v) = map.get(key) {
                return Some(v);
            }
        }
        let mut cur = scope;
        while let Some((parent, _)) = split_parent_leaf(cur) {
            let cand = format!("{parent}/{local_key}");
            if let Some(&v) = map.get(&cand) {
                return Some(v);
            }
            cur = parent;
        }
        if let Some(&v) = map.get(local_key) {
            return Some(v);
        }
    }
    None
}

pub(crate) fn resolve_ss_handle(
    raw: &str,
    scope: &str,
    domains: &[PowerDomain],
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    ss_by_name: &BTreeMap<String, SupplySetId>,
) -> Option<SupplySetId> {
    let key = trim_hier_path(raw);
    if let Some(id) = lookup_scoped(ss_by_name, key, scope) {
        return Some(id);
    }
    if let Some((pd_raw, handle)) = split_handle_dot(key) {
        if let Some(pd_id) = lookup_scoped(domain_by_name, pd_raw, scope) {
            let pd = &domains[pd_id.0 as usize];
            if let Some(bound) = pd.supply_handles.get(handle) {
                return lookup_scoped(ss_by_name, bound, &pd.scope);
            }
            if handle == "primary" {
                if let Some(prim) = &pd.primary_supply_set {
                    return lookup_scoped(ss_by_name, prim, &pd.scope);
                }
            }
            let implicit_name = format!("{}.{handle}", pd.name);
            return ss_by_name.get(&implicit_name).copied();
        }
    }
    None
}

pub(crate) fn resolve_power_state_target_ss(
    intent: &PowerIntent,
    target: &str,
    scope: &str,
) -> String {
    let mut domain_by_name: BTreeMap<String, PowerDomainId> = BTreeMap::new();
    for (i, d) in intent.domains.iter().enumerate() {
        domain_by_name.insert(d.name.clone(), PowerDomainId(i as u32));
    }
    let mut ss_by_name: BTreeMap<String, SupplySetId> = BTreeMap::new();
    for (i, ss) in intent.supply_sets.iter().enumerate() {
        ss_by_name.insert(ss.name.clone(), SupplySetId(i as u32));
    }
    if let Some(ssid) =
        resolve_ss_handle(target, scope, &intent.domains, &domain_by_name, &ss_by_name)
    {
        return intent.supply_sets[ssid.0 as usize].name.clone();
    }
    if let Some(pd_id) = lookup_scoped(&domain_by_name, target, scope) {
        let pd = &intent.domains[pd_id.0 as usize];
        if let Some(ref prim) = pd.primary_supply_set {
            return prim.trim_start_matches('/').to_string();
        }
        return crate::cpf::cpf_ss_name(&pd.name);
    }
    target.trim_start_matches('/').to_string()
}

fn ensure_supply_set(
    intent: &mut PowerIntent,
    ss_by_name: &mut BTreeMap<String, SupplySetId>,
    name: &str,
    scope: &str,
    is_implicit: bool,
    loc: Option<UpfLoc>,
) -> SupplySetId {
    if let Some(&id) = ss_by_name.get(name) {
        return id;
    }
    let id = SupplySetId(intent.supply_sets.len() as u32);
    intent.supply_sets.push(SupplySet {
        id,
        name: name.to_string(),
        scope: scope.to_string(),
        power: None,
        ground: None,
        pwell: None,
        nwell: None,
        deeppwell: None,
        deepnwell: None,
        extra_functions: BTreeMap::new(),
        is_implicit,
        reference_gnd: None,
        loc,
    });
    ss_by_name.insert(name.to_string(), id);
    id
}

fn detect_supply_cycles(intent: &mut PowerIntent, net_by_name: &BTreeMap<String, SupplyNetId>) {
    let n = intent.supply_nets.len();
    if n == 0 {
        return;
    }
    let mut adj: Vec<Vec<(usize, Option<UpfLoc>)>> = vec![Vec::new(); n];
    for psw in &intent.power_switches {
        let inputs: Vec<usize> = psw
            .input_supply_ports
            .iter()
            .filter_map(|(_, net)| {
                lookup_scoped(net_by_name, net, &psw.scope).map(|id| id.0 as usize)
            })
            .collect();
        let outputs: Vec<usize> = psw
            .output_supply_ports
            .iter()
            .filter_map(|(_, net)| {
                lookup_scoped(net_by_name, net, &psw.scope).map(|id| id.0 as usize)
            })
            .collect();
        for &u in &inputs {
            for &v in &outputs {
                adj[u].push((v, psw.loc.clone()));
            }
        }
    }

    let mut color = vec![0u8; n];
    for start in 0..n {
        if color[start] == 0 {
            dfs_cycle(start, &adj, &mut color, intent);
        }
    }
}

fn dfs_cycle(
    u: usize,
    adj: &[Vec<(usize, Option<UpfLoc>)>],
    color: &mut [u8],
    intent: &mut PowerIntent,
) {
    color[u] = 1;
    for (v, loc) in &adj[u] {
        match color[*v] {
            1 => {
                let net_u = &intent.supply_nets[u].name;
                let net_v = &intent.supply_nets[*v].name;
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_SUPPLY_CYCLE".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "supply connectivity cycle detected between `{net_u}` and `{net_v}`"
                    ),
                    loc: loc.clone(),
                });
            }
            0 => dfs_cycle(*v, adj, color, intent),
            _ => {}
        }
    }
    color[u] = 2;
}

fn check_dangling_supply_nets(
    intent: &mut PowerIntent,
    net_by_name: &BTreeMap<String, SupplyNetId>,
) {
    let mut used: BTreeSet<SupplyNetId> = BTreeSet::new();
    for ss in &intent.supply_sets {
        for opt in [
            ss.power.as_deref(),
            ss.ground.as_deref(),
            ss.pwell.as_deref(),
            ss.nwell.as_deref(),
            ss.deeppwell.as_deref(),
            ss.deepnwell.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(id) = lookup_scoped(net_by_name, opt, &ss.scope) {
                used.insert(id);
            }
        }
    }
    for psw in &intent.power_switches {
        for (_, net) in psw
            .input_supply_ports
            .iter()
            .chain(psw.output_supply_ports.iter())
        {
            if let Some(id) = lookup_scoped(net_by_name, net, &psw.scope) {
                used.insert(id);
            }
        }
    }
    for pst in &intent.pst_tables {
        for s in &pst.supplies {
            if let Some(id) = lookup_scoped(net_by_name, s, &pst.scope) {
                used.insert(id);
            }
        }
    }

    for net in &intent.supply_nets {
        let has_driver = !net.connected_ports.is_empty()
            || !net.connected_pins.is_empty()
            || !net.port_states.is_empty()
            || net.nominal_voltage.is_some();
        if !has_driver && !used.contains(&net.id) {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_SUPPLY_UNCONNECTED".to_string(),
                severity: DiagSeverity::Warning,
                message: format!(
                    "supply net `{}` has no port/switch driver and is unused",
                    net.name
                ),
                loc: net.loc.clone(),
            });
        }
    }
}

impl ResolvedPowerIntent {
    /// Convenience query returning supply net name if known.
    pub fn net_name(&self, id: SupplyNetId) -> Option<&str> {
        self.intent
            .supply_nets
            .get(id.0 as usize)
            .map(|n| n.name.as_str())
    }
}
