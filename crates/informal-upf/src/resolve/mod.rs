//! Deterministic 8-phase two-phase precedence and successive-refinement
//! resolver for canonical [`PowerIntent`].

pub mod equivalence;
pub mod membership;
pub mod precedence;
pub mod refinement;
pub mod states;
pub mod supply_graph;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ir::{
    normalize_scope, IsolationStrategy, PowerDomainId, PowerIntent, PowerStateDef,
    RetentionStrategy, SupplyNetId, SupplyPortId, SupplySetId, UpfLoc,
};
use crate::upf::ObjectFinder;

pub(crate) use membership::design_object_exists;
pub use membership::matches_any_element_in_scope;
pub use precedence::{
    endpoint_base_matches, endpoint_matches_patterns, parse_bit_slice_expr, sv_slice_to_phys_range,
    SignalEndpoint,
};
pub(crate) use supply_graph::{lookup_scoped, resolve_power_state_target_ss, resolve_ss_handle};

/// Result of querying IEEE 1801 strategy precedence for a crossing or element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyMatch<T> {
    /// No strategy matched.
    None,
    /// Matched a concrete active strategy.
    Matched(T),
    /// Matched a suppression strategy (`-no_isolation`, `-no_shift`, `-no_retention`).
    Suppressed(T),
}

impl<T> StrategyMatch<T> {
    pub fn active(&self) -> Option<&T> {
        match self {
            Self::Matched(v) => Some(v),
            Self::None | Self::Suppressed(_) => None,
        }
    }

    pub fn is_suppressed(&self) -> bool {
        matches!(self, Self::Suppressed(_))
    }
}

/// Fully resolved power intent with normalized scopes, materialized implicit
/// supply sets, DSU equivalence classes, merged strategies, and normalized states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPowerIntent {
    pub intent: PowerIntent,
    pub scope_tree: BTreeSet<String>,
    pub domain_by_name: BTreeMap<String, PowerDomainId>,
    pub ss_by_name: BTreeMap<String, SupplySetId>,
    pub net_by_name: BTreeMap<String, SupplyNetId>,
    pub port_by_name: BTreeMap<String, SupplyPortId>,
    pub domain_primary_ss: Vec<Option<SupplySetId>>,
    /// Resolved `SupplyNetId` for `(power, ground)` rails of each `SupplySetId`.
    pub ss_power_net: Vec<Option<SupplyNetId>>,
    pub ss_ground_net: Vec<Option<SupplyNetId>>,
    /// DSU representative for each `SupplySetId` (`set_equivalent` without `-function_only` + identical physical nets).
    pub equiv_class: Vec<SupplySetId>,
    /// DSU representative for functional ON/OFF equivalence (includes `-function_only`).
    pub func_equiv_class: Vec<SupplySetId>,
    /// Explicit full `set_equivalent` groups checked for voltage+simstate coherence in `pst`.
    pub explicit_equiv_groups: Vec<(Vec<SupplySetId>, Option<UpfLoc>)>,
    /// Explicit `-function_only` `set_equivalent` groups checked for simstate coherence only.
    pub function_only_equiv_groups: Vec<(Vec<SupplySetId>, Option<UpfLoc>)>,
    /// Normalized states per `SupplySetId`.
    pub ss_states: Vec<Vec<PowerStateDef>>,
    /// Global `-illegal` rules not bound to a single `SupplySetId`.
    pub global_illegal_rules: Vec<PowerStateDef>,
    /// Resolved RTL instances per domain (populated when `finder.has_design()`).
    pub domain_instances: Vec<BTreeSet<String>>,
    /// Reverse map from RTL instance path -> owning `PowerDomainId`.
    pub instance_domain: BTreeMap<String, PowerDomainId>,
    /// True if resolved against a live design `ObjectFinder`.
    pub has_design: bool,
}

/// Resolves a [`PowerIntent`] in 8 deterministic phases against `finder`.
pub fn resolve(intent: &mut PowerIntent, finder: &dyn ObjectFinder) -> ResolvedPowerIntent {
    let mut work = intent.clone();

    // Phase 1: Normalize hierarchy & collect scopes.
    let mut scope_tree = BTreeSet::new();
    scope_tree.insert(String::new());
    for pd in &mut work.domains {
        pd.scope = normalize_scope(&pd.scope, '/');
        pd.name = normalize_scope(&pd.name, '/');
        scope_tree.insert(pd.scope.clone());
    }
    for sp in &mut work.supply_ports {
        sp.scope = normalize_scope(&sp.scope, '/');
        sp.name = normalize_scope(&sp.name, '/');
        scope_tree.insert(sp.scope.clone());
    }
    for sn in &mut work.supply_nets {
        sn.scope = normalize_scope(&sn.scope, '/');
        sn.name = normalize_scope(&sn.name, '/');
        scope_tree.insert(sn.scope.clone());
    }
    for ss in &mut work.supply_sets {
        ss.scope = normalize_scope(&ss.scope, '/');
        scope_tree.insert(ss.scope.clone());
    }

    let ops = work.deferred_ops.clone();

    // Phase 2: Resolve supply ports & supply nets + cycle check.
    let (port_by_name, net_by_name) = supply_graph::resolve_supply_graph(&mut work, &ops);

    // Phase 3: Merge domain `-update` & materialize implicit supply sets + dangling check.
    membership::merge_power_domains(&mut work);
    let (domain_by_name, ss_by_name) =
        supply_graph::materialize_domain_supplies(&mut work, &ops, &net_by_name);

    // Phase 4: Validate elements & domain membership against ObjectFinder.
    let mut domain_instances = vec![BTreeSet::new(); work.domains.len()];
    let mut instance_domain = BTreeMap::new();
    if finder.has_design() {
        membership::resolve_design_membership(
            &mut work,
            finder,
            &mut domain_instances,
            &mut instance_domain,
        );
    }
    membership::validate_strategy_domains(&mut work, &domain_by_name);

    // Phase 5: Resolve deferred CPF net equivalences & `set_equivalent` Union-Find (DSU).
    equivalence::collect_cpf_equiv_nets(&mut work, &ops, &domain_by_name, &ss_by_name);
    let (equiv_class, func_equiv_class, explicit_equiv_groups, function_only_equiv_groups) =
        equivalence::build_equivalence_classes(
            &mut work,
            &domain_by_name,
            &ss_by_name,
            &net_by_name,
        );

    // Phase 6: Strategy merging & successive refinement (`-update`, `_control`, `map_*`).
    let unmerged_deferred_elems =
        refinement::apply_strategy_refinements(&mut work, &ops, &domain_by_name, &ss_by_name);
    if finder.has_design() {
        membership::validate_strategy_elements(&mut work, finder, &unmerged_deferred_elems);
    }

    // Phase 7: Check same-tier strategy overlap diagnostics.
    precedence::check_strategy_overlaps(&mut work, &domain_by_name);

    // Phase 8: Normalize supply-set states & UPF 1.0 net -> set bridges.
    let (domain_primary_ss, ss_power_net, ss_ground_net) =
        supply_graph::resolve_ss_rail_nets(&work, &domain_by_name, &ss_by_name, &net_by_name);

    let (ss_states, global_illegal_rules) = states::normalize_supply_states(
        &mut work,
        &domain_by_name,
        &ss_by_name,
        &ss_power_net,
        &ss_ground_net,
    );

    intent.diagnostics = work.diagnostics.clone();

    ResolvedPowerIntent {
        intent: work,
        scope_tree,
        domain_by_name,
        ss_by_name,
        net_by_name,
        port_by_name,
        domain_primary_ss,
        ss_power_net,
        ss_ground_net,
        equiv_class,
        func_equiv_class,
        explicit_equiv_groups,
        function_only_equiv_groups,
        ss_states,
        global_illegal_rules,
        domain_instances,
        instance_domain,
        has_design: finder.has_design(),
    }
}

impl ResolvedPowerIntent {
    /// Resolves a power domain reference `raw` in `scope` (handling `/PD`, relative, and parent-scope names).
    pub fn resolve_domain(&self, raw: &str, scope: &str) -> Option<PowerDomainId> {
        lookup_scoped(&self.domain_by_name, raw, scope)
    }

    /// Resolves a supply set or domain handle (`SS`, `/SS`, `PD.primary`, `/PD.handle`) in `scope`.
    pub fn resolve_supply_set(&self, raw: &str, scope: &str) -> Option<SupplySetId> {
        resolve_ss_handle(
            raw,
            scope,
            &self.intent.domains,
            &self.domain_by_name,
            &self.ss_by_name,
        )
    }

    /// Resolves a supply net reference `raw` in `scope` (handling `/VDD`, relative, and parent-scope names).
    pub fn resolve_supply_net(&self, raw: &str, scope: &str) -> Option<SupplyNetId> {
        lookup_scoped(&self.net_by_name, raw, scope)
    }

    /// Returns `true` if `target` matches any element pattern in `patterns` relative to `scope`.
    pub fn element_matches_in_scope(&self, patterns: &[String], scope: &str, target: &str) -> bool {
        matches_any_element_in_scope(patterns, scope, target)
    }

    /// Convenience query returning canonical power domain name if known.
    pub fn domain_name(&self, id: PowerDomainId) -> Option<&str> {
        self.intent
            .domains
            .get(id.0 as usize)
            .map(|d| d.name.as_str())
    }

    /// True if `a` and `b` belong to the same full supply equivalence class.
    pub fn same_supply_class(&self, a: SupplySetId, b: SupplySetId) -> bool {
        if a == b {
            return true;
        }
        let ca = self.equiv_class.get(a.0 as usize).copied();
        let cb = self.equiv_class.get(b.0 as usize).copied();
        ca.is_some() && ca == cb
    }

    /// True if `a` and `b` share functional ON/OFF equivalence (including `-function_only`).
    pub fn same_func_supply_class(&self, a: SupplySetId, b: SupplySetId) -> bool {
        if a == b {
            return true;
        }
        let ca = self.func_equiv_class.get(a.0 as usize).copied();
        let cb = self.func_equiv_class.get(b.0 as usize).copied();
        ca.is_some() && ca == cb
    }

    /// Resolves a domain name or handle to its primary [`SupplySetId`].
    pub fn primary_ss(&self, pd: PowerDomainId) -> Option<SupplySetId> {
        self.domain_primary_ss.get(pd.0 as usize).copied().flatten()
    }

    /// Resolves the effective isolation supply set for `iso` (explicit or
    /// `<PD>.default_isolation` or `<PD>.primary`).
    pub fn effective_iso_supply(&self, iso: &IsolationStrategy) -> Option<SupplySetId> {
        if let Some(ref s) = iso.isolation_supply_set {
            if let Some(id) = resolve_ss_handle(
                s,
                &iso.scope,
                &self.intent.domains,
                &self.domain_by_name,
                &self.ss_by_name,
            ) {
                return Some(id);
            }
        }
        if let Some(pd_id) = lookup_scoped(&self.domain_by_name, &iso.domain, &iso.scope) {
            let pd = &self.intent.domains[pd_id.0 as usize];
            if let Some(def_iso) = pd.supply_handles.get("default_isolation") {
                if let Some(id) = resolve_ss_handle(
                    def_iso,
                    &pd.scope,
                    &self.intent.domains,
                    &self.domain_by_name,
                    &self.ss_by_name,
                ) {
                    return Some(id);
                }
            }
            return self.primary_ss(pd_id);
        }
        None
    }

    /// Resolves the effective retention supply set for `ret` (explicit or
    /// `<PD>.default_retention` or `<PD>.primary`).
    pub fn effective_ret_supply(&self, ret: &RetentionStrategy) -> Option<SupplySetId> {
        if let Some(ref s) = ret.retention_supply_set {
            if let Some(id) = resolve_ss_handle(
                s,
                &ret.scope,
                &self.intent.domains,
                &self.domain_by_name,
                &self.ss_by_name,
            ) {
                return Some(id);
            }
        }
        if let Some(pd_id) = lookup_scoped(&self.domain_by_name, &ret.domain, &ret.scope) {
            let pd = &self.intent.domains[pd_id.0 as usize];
            if let Some(def_ret) = pd.supply_handles.get("default_retention") {
                if let Some(id) = resolve_ss_handle(
                    def_ret,
                    &pd.scope,
                    &self.intent.domains,
                    &self.domain_by_name,
                    &self.ss_by_name,
                ) {
                    return Some(id);
                }
            }
            return self.primary_ss(pd_id);
        }
        None
    }
}
