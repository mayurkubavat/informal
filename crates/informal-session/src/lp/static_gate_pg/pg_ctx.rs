//! Shared PG-net resolution and effective pin supply context for Stage 3
//! (`netlist`) and Stage 4 (`pg`) checks.

use std::collections::{BTreeSet, HashMap, HashSet};

use informal_liberty::{LibertyDb, PgType};
use informal_svlang::struct_graph::{
    CellId, DriverOrigin, ExprOpSummary, NetSegId, PinId, StructuralGraph,
};
use informal_upf::{
    LsRequirement, PowerDomainId, PstSat, ResolvedPowerIntent, SupplyNetId, SupplySetId,
};

use crate::lp::{rules, LpSeverity, LpStatus, LpViolation};

/// Resolved canonical power/ground identity of a physical net or PG pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgResolved {
    Single(SupplyNetId),
    Short(Vec<SupplyNetId>),
    Const { is_zero: bool, is_all_ones: bool },
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinPgState {
    Unconnected,
    Connected(PgResolved),
}

/// Shared analysis context for Stage 3 (`netlist`) and Stage 4 (`pg`).
pub struct GatePgCtx<'a> {
    pub graph: &'a StructuralGraph,
    pub resolved: &'a ResolvedPowerIntent,
    pub liberty: &'a LibertyDb,
    pub cell_domains: Vec<Option<PowerDomainId>>,
    pub sn_by_name: HashMap<String, SupplyNetId>,
    pub ss_by_power_net: HashMap<SupplyNetId, SupplySetId>,
    pub supply_can_be_off: HashMap<SupplyNetId, bool>,
    pub ss_can_be_off: HashMap<SupplySetId, bool>,
    pub net_pg_cache: HashMap<NetSegId, PgResolved>,
    pub any_pg_connected: bool,
}

impl<'a> GatePgCtx<'a> {
    pub fn new(
        graph: &'a StructuralGraph,
        resolved: &'a ResolvedPowerIntent,
        liberty: &'a LibertyDb,
        sat: &mut PstSat,
    ) -> Self {
        let cell_domains = crate::lp::static_rtl::assign_cell_domains(graph, resolved);

        let mut sn_by_name = HashMap::new();
        for (idx, sn) in resolved.intent.supply_nets.iter().enumerate() {
            let id = SupplyNetId(idx as u32);
            sn_by_name.insert(sn.name.clone(), id);
            if let Some(leaf) = sn.name.rsplit('/').next() {
                sn_by_name.entry(leaf.to_string()).or_insert(id);
            }
        }

        let mut ss_by_power_net = HashMap::new();
        let mut ss_can_be_off = HashMap::new();
        let mut supply_can_be_off = HashMap::new();
        for (idx, pnet_opt) in resolved.ss_power_net.iter().enumerate() {
            let ssid = SupplySetId(idx as u32);
            let off = sat.can_be_off_while_off(ssid, ssid).is_some();
            ss_can_be_off.insert(ssid, off);
            if let Some(pnet) = pnet_opt {
                ss_by_power_net.entry(*pnet).or_insert(ssid);
                let prev = supply_can_be_off.get(pnet).copied().unwrap_or(false);
                supply_can_be_off.insert(*pnet, prev || off);
            }
        }

        let mut any_pg_connected = false;
        'scan: for c in &graph.cells {
            if let Some(lib_c) = liberty.cell(&c.module) {
                for pg in &lib_c.pg_pins {
                    if let Some(&pid) = c.pins.iter().find(|&&pid| graph.pin(pid).name == pg.name) {
                        let p = graph.pin(pid);
                        if !p.unconnected && !p.external_conns.is_empty() {
                            any_pg_connected = true;
                            break 'scan;
                        }
                    }
                }
            }
        }

        Self {
            graph,
            resolved,
            liberty,
            cell_domains,
            sn_by_name,
            ss_by_power_net,
            supply_can_be_off,
            ss_can_be_off,
            net_pg_cache: HashMap::with_capacity(256),
            any_pg_connected,
        }
    }

    pub fn sn_name(&self, id: SupplyNetId) -> &str {
        &self.resolved.intent.supply_nets[id.0 as usize].name
    }

    pub fn domain_name(&self, pd: Option<PowerDomainId>) -> Option<String> {
        pd.map(|id| self.resolved.intent.domains[id.0 as usize].name.clone())
    }

    pub fn domain_primary_ss(&self, pd: Option<PowerDomainId>) -> Option<SupplySetId> {
        pd.and_then(|id| self.resolved.primary_ss(id))
    }

    pub fn domain_primary_power(&self, pd: Option<PowerDomainId>) -> Option<SupplyNetId> {
        let ss = self.domain_primary_ss(pd)?;
        self.resolved
            .ss_power_net
            .get(ss.0 as usize)
            .copied()
            .flatten()
    }

    pub fn domain_primary_ground(&self, pd: Option<PowerDomainId>) -> Option<SupplyNetId> {
        let ss = self.domain_primary_ss(pd)?;
        self.resolved
            .ss_ground_net
            .get(ss.0 as usize)
            .copied()
            .flatten()
    }

    pub fn domain_bias_net(&self, pd: Option<PowerDomainId>, func: &str) -> Option<SupplyNetId> {
        let ss = self.domain_primary_ss(pd)?;
        let sset = &self.resolved.intent.supply_sets[ss.0 as usize];
        let net_name = match func {
            "nwell" => sset.nwell.as_deref(),
            "pwell" => sset.pwell.as_deref(),
            "deepnwell" => sset.deepnwell.as_deref(),
            "deeppwell" => sset.deeppwell.as_deref(),
            other => sset.extra_functions.get(other).map(String::as_str),
        }?;
        self.sn_by_name
            .get(net_name)
            .or_else(|| {
                net_name
                    .rsplit('/')
                    .next()
                    .and_then(|l| self.sn_by_name.get(l))
            })
            .copied()
    }

    pub fn is_supply_switchable(&self, sn: SupplyNetId) -> bool {
        if self.supply_can_be_off.get(&sn).copied().unwrap_or(false) {
            return true;
        }
        let name = self.sn_name(sn);
        self.resolved.intent.power_switches.iter().any(|sw| {
            sw.output_supply_ports
                .iter()
                .any(|(_, out_n)| out_n == name || out_n.rsplit('/').next() == Some(name))
        })
    }

    pub fn is_ss_switchable(&self, ss: SupplySetId) -> bool {
        if self.ss_can_be_off.get(&ss).copied().unwrap_or(false) {
            return true;
        }
        if let Some(pnet) = self
            .resolved
            .ss_power_net
            .get(ss.0 as usize)
            .copied()
            .flatten()
        {
            return self.is_supply_switchable(pnet);
        }
        false
    }

    /// Resolves a net segment to its canonical UPF supply net(s) by following
    /// continuous assignments and hierarchical port connections (memoized).
    pub fn resolve_net_pg(&mut self, nid: NetSegId) -> PgResolved {
        if let Some(cached) = self.net_pg_cache.get(&nid) {
            return cached.clone();
        }
        let mut visiting = HashSet::new();
        let res = self.resolve_net_pg_inner(nid, &mut visiting, 0);
        self.net_pg_cache.insert(nid, res.clone());
        res
    }

    fn resolve_net_pg_inner(
        &mut self,
        nid: NetSegId,
        visiting: &mut HashSet<NetSegId>,
        depth: usize,
    ) -> PgResolved {
        if depth > 16 || !visiting.insert(nid) {
            return PgResolved::Unresolved;
        }
        let net = self.graph.net(nid);
        let mut found_sns: BTreeSet<SupplyNetId> = BTreeSet::new();
        let mut saw_const: Option<(bool, bool)> = None;
        let mut saw_compound = false;

        let drivers = net.drivers.clone();
        for drv in &drivers {
            match &drv.origin {
                DriverOrigin::PrimaryInput(_) => {
                    if let Some(&sn) = self
                        .sn_by_name
                        .get(&net.upf_path)
                        .or_else(|| self.sn_by_name.get(&net.name))
                    {
                        found_sns.insert(sn);
                    }
                }
                DriverOrigin::Assign | DriverOrigin::ParentPort(_) | DriverOrigin::NetAlias => {
                    if let ExprOpSummary::Const {
                        is_zero,
                        is_all_ones,
                    } = drv.op
                    {
                        saw_const = Some((is_zero, is_all_ones));
                    }
                    if let Some(src) = drv.src_net {
                        self.merge_pg(src, visiting, depth + 1, &mut found_sns);
                    }
                    if !drv.extra_deps.is_empty() {
                        saw_compound = true;
                        for &(dep, _) in &drv.extra_deps {
                            self.merge_pg(dep, visiting, depth + 1, &mut found_sns);
                        }
                    }
                }
                DriverOrigin::ChildPin(pid) => {
                    let pin = self.graph.pin(*pid);
                    let child = self.graph.cell(pin.cell);
                    if let Some(lib_c) = self.liberty.cell(&child.module) {
                        if lib_c.is_power_switch() {
                            if let Some(&sn) = self
                                .sn_by_name
                                .get(&net.upf_path)
                                .or_else(|| self.sn_by_name.get(&net.name))
                            {
                                found_sns.insert(sn);
                            }
                        }
                    } else if let Some(src) = drv.src_net {
                        self.merge_pg(src, visiting, depth + 1, &mut found_sns);
                    }
                }
                _ => {}
            }
        }
        visiting.remove(&nid);

        if found_sns.len() >= 2 {
            return PgResolved::Short(found_sns.into_iter().collect());
        }
        if let Some(&only) = found_sns.iter().next() {
            return PgResolved::Single(only);
        }
        if !saw_compound {
            if let Some(&sn) = self
                .sn_by_name
                .get(&net.upf_path)
                .or_else(|| self.sn_by_name.get(&net.name))
            {
                return PgResolved::Single(sn);
            }
        }
        if let Some((is_zero, is_all_ones)) = saw_const {
            return PgResolved::Const {
                is_zero,
                is_all_ones,
            };
        }
        PgResolved::Unresolved
    }

    fn merge_pg(
        &mut self,
        nid: NetSegId,
        visiting: &mut HashSet<NetSegId>,
        depth: usize,
        out: &mut BTreeSet<SupplyNetId>,
    ) {
        match self.resolve_net_pg_inner(nid, visiting, depth) {
            PgResolved::Single(sn) => {
                out.insert(sn);
            }
            PgResolved::Short(sns) => {
                out.extend(sns);
            }
            PgResolved::Const { .. } | PgResolved::Unresolved => {}
        }
    }

    pub fn pin_pg_state(&mut self, cid: CellId, pg_name: &str) -> PinPgState {
        let cell = self.graph.cell(cid);
        let Some(&pid) = cell
            .pins
            .iter()
            .find(|&&pid| self.graph.pin(pid).name == pg_name)
        else {
            return PinPgState::Unconnected;
        };
        let pin = self.graph.pin(pid);
        if pin.unconnected || pin.external_conns.is_empty() {
            return PinPgState::Unconnected;
        }
        let mut found: BTreeSet<SupplyNetId> = BTreeSet::new();
        let mut const_val: Option<(bool, bool)> = None;
        let conns = pin.external_conns.clone();
        for bnd in &conns {
            if let Some(pnet) = bnd.parent_net {
                match self.resolve_net_pg(pnet) {
                    PgResolved::Single(sn) => {
                        found.insert(sn);
                    }
                    PgResolved::Short(sns) => {
                        found.extend(sns);
                    }
                    PgResolved::Const {
                        is_zero,
                        is_all_ones,
                    } => const_val = Some((is_zero, is_all_ones)),
                    PgResolved::Unresolved => {}
                }
            } else if !bnd.expr_deps.is_empty() {
                for &(dep, _) in &bnd.expr_deps {
                    match self.resolve_net_pg(dep) {
                        PgResolved::Single(sn) => {
                            found.insert(sn);
                        }
                        PgResolved::Short(sns) => {
                            found.extend(sns);
                        }
                        PgResolved::Const { .. } | PgResolved::Unresolved => {}
                    }
                }
            } else if let ExprOpSummary::Const {
                is_zero,
                is_all_ones,
            } = bnd.expr_op
            {
                const_val = Some((is_zero, is_all_ones));
            }
        }
        if found.len() >= 2 {
            PinPgState::Connected(PgResolved::Short(found.into_iter().collect()))
        } else if let Some(&only) = found.iter().next() {
            PinPgState::Connected(PgResolved::Single(only))
        } else if let Some((is_zero, is_all_ones)) = const_val {
            PinPgState::Connected(PgResolved::Const {
                is_zero,
                is_all_ones,
            })
        } else {
            PinPgState::Connected(PgResolved::Unresolved)
        }
    }

    /// Resolves the effective [`SupplySetId`] of a signal pin on `cid`,
    /// honoring Liberty `related_power_pin` -> physical PG net when available.
    pub fn effective_pin_ss(&mut self, cid: CellId, pin_name: &str) -> Option<SupplySetId> {
        let module = self.graph.cell(cid).module.clone();
        if let Some(lib_c) = self.liberty.cell(&module) {
            let rel_pwr = lib_c
                .pin(pin_name)
                .and_then(|p| p.related_power_pin.clone());
            if let Some(pg_name) = rel_pwr {
                if let PinPgState::Connected(PgResolved::Single(sn)) =
                    self.pin_pg_state(cid, &pg_name)
                {
                    if let Some(&ss) = self.ss_by_power_net.get(&sn) {
                        return Some(ss);
                    }
                }
            }
            if let Some(pg) = lib_c
                .pg_pins
                .iter()
                .find(|p| p.pg_type == PgType::PrimaryPower)
            {
                let pg_name = pg.name.clone();
                if let PinPgState::Connected(PgResolved::Single(sn)) =
                    self.pin_pg_state(cid, &pg_name)
                {
                    if let Some(&ss) = self.ss_by_power_net.get(&sn) {
                        return Some(ss);
                    }
                }
            }
        }
        self.domain_primary_ss(self.cell_domains[cid.0 as usize])
    }

    /// Returns true if `cid`'s power supply can be powered off in a legal PST state.
    pub fn cell_power_can_be_off(&mut self, cid: CellId) -> bool {
        let module = self.graph.cell(cid).module.clone();
        if let Some(lib_c) = self.liberty.cell(&module) {
            if let Some(pg) = lib_c
                .pg_pins
                .iter()
                .find(|p| matches!(p.pg_type, PgType::PrimaryPower | PgType::BackupPower))
            {
                let pg_name = pg.name.clone();
                if let PinPgState::Connected(PgResolved::Single(sn)) =
                    self.pin_pg_state(cid, &pg_name)
                {
                    return self.is_supply_switchable(sn);
                }
            }
        }
        self.domain_primary_ss(self.cell_domains[cid.0 as usize])
            .is_some_and(|ss| self.is_ss_switchable(ss))
    }
}

pub fn needs_level_shifter(
    sat: &mut PstSat,
    src_ss: SupplySetId,
    dst_ss: SupplySetId,
    tol_uv: i64,
) -> bool {
    matches!(
        sat.voltage_relation(src_ss, dst_ss, tol_uv),
        LsRequirement::LowToHigh { .. }
            | LsRequirement::HighToLow { .. }
            | LsRequirement::Both { .. }
    )
}

pub fn pin_parent_nets(graph: &StructuralGraph, pid: PinId) -> Vec<NetSegId> {
    let mut out = Vec::new();
    let pin = graph.pin(pid);
    for bnd in &pin.external_conns {
        if let Some(n) = bnd.parent_net {
            out.push(n);
        } else {
            for &(dn, _) in &bnd.expr_deps {
                out.push(dn);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn push_violation(
    out: &mut Vec<LpViolation>,
    rule_id: &str,
    stage: &str,
    domain_src: Option<String>,
    domain_dst: Option<String>,
    signal: String,
    message: String,
    rtl_loc: Option<String>,
) {
    let severity = rules::lookup_rule(rule_id)
        .map(|r| r.default_severity)
        .unwrap_or(LpSeverity::Error);
    out.push(LpViolation {
        id: 0,
        rule_id: rule_id.to_string(),
        severity,
        stage: stage.to_string(),
        domain_src,
        domain_dst,
        signal,
        width_bits: 1,
        raw_crossings: 1,
        message,
        upf_loc: None,
        rtl_loc,
        pst_witness: None,
        status: LpStatus::Open,
        waiver_id: None,
        formal_prop: None,
        trace: None,
        auto_formal_meta: None,
    });
}
