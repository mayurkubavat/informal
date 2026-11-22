//! `set_equivalent` union-find DSU and rail-signature equivalence grouping.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{
    DeferredOp, DiagSeverity, PowerDomainId, PowerIntent, SupplyEquivalence, SupplyNetId,
    SupplySet, SupplySetId, UpfDiag, UpfLoc,
};

use super::supply_graph::{lookup_scoped, resolve_ss_handle};

pub(crate) type EquivGroups = Vec<(Vec<SupplySetId>, Option<UpfLoc>)>;

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            let lo = ra.min(rb);
            let hi = ra.max(rb);
            self.parent[hi] = lo;
        }
    }
}

pub(crate) fn collect_cpf_equiv_nets(
    work: &mut PowerIntent,
    ops: &[DeferredOp],
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    ss_by_name: &BTreeMap<String, SupplySetId>,
) {
    for op in ops {
        if let DeferredOp::CpfEquivalentNets {
            domain,
            scope,
            is_ground,
            nets,
            function_only,
            loc,
        } = op
        {
            let mut final_nets = nets.clone();
            if let Some(pd_id) = lookup_scoped(domain_by_name, domain, scope) {
                let pd = &work.domains[pd_id.0 as usize];
                let ss_primary = pd
                    .primary_supply_set
                    .as_deref()
                    .and_then(|s| {
                        resolve_ss_handle(s, &pd.scope, &work.domains, domain_by_name, ss_by_name)
                    })
                    .map(|ssid| &work.supply_sets[ssid.0 as usize]);
                let primary_net = if *is_ground {
                    ss_primary
                        .and_then(|ss| ss.ground.clone())
                        .or_else(|| pd.legacy_primary_ground_net.clone())
                } else {
                    ss_primary
                        .and_then(|ss| ss.power.clone())
                        .or_else(|| pd.legacy_primary_power_net.clone())
                };
                if let Some(p) = primary_net {
                    if !final_nets.contains(&p) {
                        final_nets.insert(0, p);
                    }
                }
            }
            work.equivalences.push(SupplyEquivalence {
                sets: Vec::new(),
                nets: final_nets,
                function_only: *function_only,
                scope: scope.clone(),
                loc: loc.clone(),
            });
        }
    }
}

fn ss_rail_signature(
    ss: &SupplySet,
    net_by_name: &BTreeMap<String, SupplyNetId>,
    net_dsu: &mut Dsu,
) -> Option<Vec<(String, usize)>> {
    let mut sig: Vec<(String, usize)> = Vec::new();
    let std_rails = [
        ("power", ss.power.as_deref()),
        ("ground", ss.ground.as_deref()),
        ("pwell", ss.pwell.as_deref()),
        ("nwell", ss.nwell.as_deref()),
        ("deeppwell", ss.deeppwell.as_deref()),
        ("deepnwell", ss.deepnwell.as_deref()),
    ];
    for (func, opt) in std_rails {
        if let Some(nm) = opt {
            let nid = lookup_scoped(net_by_name, nm, &ss.scope)?;
            sig.push((func.to_string(), net_dsu.find(nid.0 as usize)));
        }
    }
    for (func, nm) in &ss.extra_functions {
        let nid = lookup_scoped(net_by_name, nm, &ss.scope)?;
        sig.push((func.clone(), net_dsu.find(nid.0 as usize)));
    }
    if sig.is_empty() {
        None
    } else {
        sig.sort();
        Some(sig)
    }
}

fn ss_touches_any_net(
    ss: &SupplySet,
    net_by_name: &BTreeMap<String, SupplyNetId>,
    nets: &BTreeSet<SupplyNetId>,
) -> bool {
    let std_rails = [
        ss.power.as_deref(),
        ss.ground.as_deref(),
        ss.pwell.as_deref(),
        ss.nwell.as_deref(),
        ss.deeppwell.as_deref(),
        ss.deepnwell.as_deref(),
    ];
    std_rails
        .into_iter()
        .flatten()
        .chain(ss.extra_functions.values().map(String::as_str))
        .filter_map(|nm| lookup_scoped(net_by_name, nm, &ss.scope))
        .any(|nid| nets.contains(&nid))
}

pub(crate) fn build_equivalence_classes(
    intent: &mut PowerIntent,
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    ss_by_name: &BTreeMap<String, SupplySetId>,
    net_by_name: &BTreeMap<String, SupplyNetId>,
) -> (Vec<SupplySetId>, Vec<SupplySetId>, EquivGroups, EquivGroups) {
    let n = intent.supply_sets.len();
    let num_nets = intent.supply_nets.len();
    let mut dsu = Dsu::new(n);
    let mut func_dsu = Dsu::new(n);
    let mut net_dsu = Dsu::new(num_nets);
    let mut func_net_dsu = Dsu::new(num_nets);
    let mut explicit_groups = Vec::new();
    let mut func_only_groups = Vec::new();
    let mut full_eq_nets: BTreeSet<SupplyNetId> = BTreeSet::new();
    let mut full_eq_net_loc: Option<UpfLoc> = None;
    let mut func_eq_nets: BTreeSet<SupplyNetId> = BTreeSet::new();
    let mut func_eq_net_loc: Option<UpfLoc> = None;

    for eq in &intent.equivalences {
        let mut group = Vec::new();
        for s in &eq.sets {
            if let Some(ssid) =
                resolve_ss_handle(s, &eq.scope, &intent.domains, domain_by_name, ss_by_name)
            {
                group.push(ssid);
            } else {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!("set_equivalent: unknown supply set `{s}`"),
                    loc: eq.loc.clone(),
                });
            }
        }
        if group.len() >= 2 {
            let rep = group[0].0 as usize;
            for &other in &group[1..] {
                func_dsu.union(rep, other.0 as usize);
                if !eq.function_only {
                    dsu.union(rep, other.0 as usize);
                }
            }
            if eq.function_only {
                func_only_groups.push((group, eq.loc.clone()));
            } else {
                explicit_groups.push((group, eq.loc.clone()));
            }
        }
        if eq.nets.len() >= 2 {
            let mut resolved_nets = Vec::new();
            for nm in &eq.nets {
                if let Some(nid) = lookup_scoped(net_by_name, nm, &eq.scope) {
                    resolved_nets.push(nid);
                } else {
                    intent.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!("set_equivalent: unknown supply net `{nm}`"),
                        loc: eq.loc.clone(),
                    });
                }
            }
            if resolved_nets.len() >= 2 {
                let rep = resolved_nets[0].0 as usize;
                for &other in &resolved_nets[1..] {
                    func_net_dsu.union(rep, other.0 as usize);
                    if !eq.function_only {
                        net_dsu.union(rep, other.0 as usize);
                    }
                }
                if eq.function_only {
                    if func_eq_net_loc.is_none() {
                        func_eq_net_loc = eq.loc.clone();
                    }
                    func_eq_nets.extend(resolved_nets);
                } else {
                    if full_eq_net_loc.is_none() {
                        full_eq_net_loc = eq.loc.clone();
                    }
                    full_eq_nets.extend(resolved_nets);
                }
            }
        }
    }

    let mut by_full_sig: BTreeMap<Vec<(String, usize)>, Vec<usize>> = BTreeMap::new();
    for (idx, ss) in intent.supply_sets.iter().enumerate() {
        if let Some(sig) = ss_rail_signature(ss, net_by_name, &mut net_dsu) {
            by_full_sig.entry(sig).or_default().push(idx);
        }
    }
    for indices in by_full_sig.values() {
        if indices.len() >= 2 {
            let first = indices[0];
            for &idx in &indices[1..] {
                dsu.union(first, idx);
                func_dsu.union(first, idx);
            }
            if indices
                .iter()
                .any(|&i| ss_touches_any_net(&intent.supply_sets[i], net_by_name, &full_eq_nets))
            {
                explicit_groups.push((
                    indices.iter().map(|&i| SupplySetId(i as u32)).collect(),
                    full_eq_net_loc.clone(),
                ));
            }
        }
    }

    if !func_eq_nets.is_empty() {
        let mut by_func_sig: BTreeMap<Vec<(String, usize)>, Vec<usize>> = BTreeMap::new();
        for (idx, ss) in intent.supply_sets.iter().enumerate() {
            if let Some(sig) = ss_rail_signature(ss, net_by_name, &mut func_net_dsu) {
                by_func_sig.entry(sig).or_default().push(idx);
            }
        }
        for indices in by_func_sig.values() {
            if indices.len() >= 2 {
                let first = indices[0];
                for &idx in &indices[1..] {
                    func_dsu.union(first, idx);
                }
                if indices.iter().any(|&i| {
                    ss_touches_any_net(&intent.supply_sets[i], net_by_name, &func_eq_nets)
                }) {
                    func_only_groups.push((
                        indices.iter().map(|&i| SupplySetId(i as u32)).collect(),
                        func_eq_net_loc.clone(),
                    ));
                }
            }
        }
    }

    let equiv_class = (0..n).map(|i| SupplySetId(dsu.find(i) as u32)).collect();
    let func_equiv_class = (0..n)
        .map(|i| SupplySetId(func_dsu.find(i) as u32))
        .collect();
    (
        equiv_class,
        func_equiv_class,
        explicit_groups,
        func_only_groups,
    )
}
