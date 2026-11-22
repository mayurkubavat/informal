//! Stage 2 Static Power-Intent & Structural Verification (`-stage rtl|design`): RTL + UPF crossing analysis
//! over `StructuralGraph` and symbolic `PstSat`.

mod feedthrough_iso;
pub mod iso;
pub mod ls;
mod repeater;
pub mod ret_psw;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use informal_svlang::struct_graph::{
    map_range_list_offset, split_crossing_group_by_cuts, BitRange, CellId, CrossingGroup, NetSegId,
    StructuralGraph,
};
use informal_upf::{
    endpoint_base_matches, parse_bit_slice_expr, sv_slice_to_phys_range, AppliesTo,
    IsolationStrategy, LevelShifterStrategy, PowerDomainId, PstSat, PstWitness,
    ResolvedPowerIntent, SignalEndpoint, StrategyMatch, SupplySetId, UpfLoc,
};

use super::finder_bridge::DesignFinder;
use super::rules::lookup_rule;
use super::{
    ensure_resolved_with_finder, static_upf, CheckLpOpts, LpBenchStats, LpSeverity, LpStatus,
    LpViolation,
};
use crate::session::Session;

pub(crate) struct Stage2Ctx<'a> {
    pub graph: &'a StructuralGraph,
    pub resolved: &'a ResolvedPowerIntent,
    pub sat: &'a mut PstSat,
    pub cell_domain: Vec<Option<PowerDomainId>>,
    pub tol_uv: i64,
    pub top_ss: Option<SupplySetId>,
}

impl<'a> Stage2Ctx<'a> {
    pub fn domain_of_cell(&self, cid: CellId) -> Option<PowerDomainId> {
        self.cell_domain.get(cid.0 as usize).copied().flatten()
    }

    pub fn domain_name(&self, pd: PowerDomainId) -> String {
        self.resolved.intent.domains[pd.0 as usize].name.clone()
    }

    pub fn ss_of_cell(&self, cid: CellId) -> Option<SupplySetId> {
        let pd = self.domain_of_cell(cid)?;
        self.resolved.primary_ss(pd)
    }

    pub fn match_iso_for_group(
        &self,
        src_pd: Option<PowerDomainId>,
        dst_pd: Option<PowerDomainId>,
        group: &CrossingGroup,
    ) -> StrategyMatch<&'a IsolationStrategy> {
        let src_rngs: Vec<(u32, u32)> = group.src_ranges.iter().map(|r| (r.lo, r.hi)).collect();
        let dst_rngs: Vec<(u32, u32)> = group.dst_ranges.iter().map(|r| (r.lo, r.hi)).collect();
        let src_net = self.graph.net(group.src_base_net);
        let dst_net = self.graph.net(group.dst_base_net);
        let src_esc = sv_escaped_upf_variant(&src_net.upf_path);
        let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
        let mut src_eps = vec![SignalEndpoint {
            upf_path: &src_net.upf_path,
            full_path: &src_net.full_path,
            esc_upf_path: src_esc.as_deref(),
            width: src_net.width,
            packed_bounds: src_net.packed_bounds,
            ranges: &src_rngs,
        }];
        let mut dst_eps = vec![SignalEndpoint {
            upf_path: &dst_net.upf_path,
            full_path: &dst_net.full_path,
            esc_upf_path: dst_esc.as_deref(),
            width: dst_net.width,
            packed_bounds: dst_net.packed_bounds,
            ranges: &dst_rngs,
        }];
        let bpin_esc;
        let bpin_full;
        if let Some(bp) = group.boundary_pin {
            let bpin = self.graph.pin(bp);
            bpin_esc = sv_escaped_upf_variant(&bpin.upf_path);
            bpin_full = pin_full_path(self.graph, bp);
            let is_dst = bpin.cell == group.dst_cell && bpin.cell != group.src_cell;
            let ep = SignalEndpoint {
                upf_path: &bpin.upf_path,
                full_path: &bpin_full,
                esc_upf_path: bpin_esc.as_deref(),
                width: bpin.width,
                packed_bounds: bpin.packed_bounds,
                ranges: if is_dst { &dst_rngs } else { &src_rngs },
            };
            if is_dst {
                dst_eps.push(ep);
            } else {
                src_eps.push(ep);
            }
        }
        self.resolved
            .match_isolation_crossing(src_pd, dst_pd, &src_eps, &dst_eps, AppliesTo::Both)
    }

    pub fn match_ls_for_group(
        &self,
        src_pd: Option<PowerDomainId>,
        dst_pd: Option<PowerDomainId>,
        group: &CrossingGroup,
    ) -> StrategyMatch<&'a LevelShifterStrategy> {
        let src_rngs: Vec<(u32, u32)> = group.src_ranges.iter().map(|r| (r.lo, r.hi)).collect();
        let dst_rngs: Vec<(u32, u32)> = group.dst_ranges.iter().map(|r| (r.lo, r.hi)).collect();
        let src_net = self.graph.net(group.src_base_net);
        let dst_net = self.graph.net(group.dst_base_net);
        let src_esc = sv_escaped_upf_variant(&src_net.upf_path);
        let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
        let mut src_eps = vec![SignalEndpoint {
            upf_path: &src_net.upf_path,
            full_path: &src_net.full_path,
            esc_upf_path: src_esc.as_deref(),
            width: src_net.width,
            packed_bounds: src_net.packed_bounds,
            ranges: &src_rngs,
        }];
        let mut dst_eps = vec![SignalEndpoint {
            upf_path: &dst_net.upf_path,
            full_path: &dst_net.full_path,
            esc_upf_path: dst_esc.as_deref(),
            width: dst_net.width,
            packed_bounds: dst_net.packed_bounds,
            ranges: &dst_rngs,
        }];
        let bpin_esc;
        let bpin_full;
        if let Some(bp) = group.boundary_pin {
            let bpin = self.graph.pin(bp);
            bpin_esc = sv_escaped_upf_variant(&bpin.upf_path);
            bpin_full = pin_full_path(self.graph, bp);
            let is_dst = bpin.cell == group.dst_cell && bpin.cell != group.src_cell;
            let ep = SignalEndpoint {
                upf_path: &bpin.upf_path,
                full_path: &bpin_full,
                esc_upf_path: bpin_esc.as_deref(),
                width: bpin.width,
                packed_bounds: bpin.packed_bounds,
                ranges: if is_dst { &dst_rngs } else { &src_rngs },
            };
            if is_dst {
                dst_eps.push(ep);
            } else {
                src_eps.push(ep);
            }
        }
        self.resolved.match_level_shifter_crossing(
            src_pd,
            dst_pd,
            &src_eps,
            &dst_eps,
            AppliesTo::Both,
        )
    }
}

pub(crate) fn strat_ref<'a, T>(m: &StrategyMatch<&'a T>) -> Option<&'a T> {
    match m {
        StrategyMatch::Matched(v) | StrategyMatch::Suppressed(v) => Some(*v),
        StrategyMatch::None => None,
    }
}

pub(crate) fn iso_rank(iso: &IsolationStrategy) -> (u8, u32) {
    let tier = if !iso.elements.is_empty() {
        4
    } else {
        match (iso.source_filter.is_some(), iso.sink_filter.is_some()) {
            (true, true) => 3,
            (true, false) | (false, true) => 2,
            (false, false) => 1,
        }
    };
    (tier, iso.precedence_order)
}

pub(crate) struct ViolationSink {
    pub list: Vec<LpViolation>,
    seen: HashSet<(String, Option<String>, Option<String>, String)>,
}

impl ViolationSink {
    pub fn new() -> Self {
        Self {
            list: Vec::new(),
            seen: HashSet::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        rule_id: &str,
        severity_override: Option<LpSeverity>,
        domain_src: Option<String>,
        domain_dst: Option<String>,
        signal: String,
        width_bits: u32,
        raw_crossings: usize,
        message: String,
        upf_loc: Option<UpfLoc>,
        rtl_loc: Option<String>,
        pst_witness: Option<PstWitness>,
    ) {
        let key = (
            rule_id.to_string(),
            domain_src.clone(),
            domain_dst.clone(),
            signal.clone(),
        );
        if !self.seen.insert(key) {
            return;
        }
        let meta = lookup_rule(rule_id);
        let severity = severity_override
            .or_else(|| meta.map(|m| m.default_severity))
            .unwrap_or(LpSeverity::Error);
        let stage = meta
            .map(|m| m.stage.word().to_string())
            .unwrap_or_else(|| "rtl".to_string());
        self.list.push(LpViolation {
            id: self.list.len() + 1,
            rule_id: rule_id.to_string(),
            severity,
            stage,
            domain_src,
            domain_dst,
            signal,
            width_bits: width_bits.max(1),
            raw_crossings: raw_crossings.max(1),
            message,
            upf_loc,
            rtl_loc,
            pst_witness,
            status: LpStatus::Open,
            waiver_id: None,
            formal_prop: None,
            trace: None,
            auto_formal_meta: None,
        });
    }
}

pub fn run_stage2(s: &mut Session, opts: &CheckLpOpts) -> Result<(), String> {
    if !s.elaborated {
        return Err("check_lp: elaborate design first".to_string());
    }
    let compiled = s
        .compiled
        .clone()
        .ok_or_else(|| "check_lp: elaborate design first (no compiled design)".to_string())?;

    let t_sg = Instant::now();
    let graph = compiled.struct_graph();
    let struct_graph_build_ms = compiled.struct_graph_build_ms();
    let _ = t_sg;

    let finder = DesignFinder::from_compiled(Some(&compiled), s.design.top.as_deref());
    let resolved = ensure_resolved_with_finder(s, &finder)?;

    let t_sat = Instant::now();
    let mut sat = PstSat::build_with_width_provider(&resolved, opts.tol_uv, &finder);
    let pst_sat_build_ms = t_sat.elapsed().as_secs_f64() * 1000.0;
    let (cnf_vars, cnf_clauses) = sat.cnf_stats();

    let t_s1 = Instant::now();
    let mut violations = static_upf::run_stage1(&resolved, &mut sat);
    let stage1_ms = t_s1.elapsed().as_secs_f64() * 1000.0;

    let t_s2 = Instant::now();
    let cell_domain = assign_cell_domains(graph, &resolved);
    let top_ss = graph
        .tops
        .first()
        .and_then(|&cid| cell_domain[cid.0 as usize])
        .and_then(|pd| resolved.primary_ss(pd));

    let raw_edges = graph.extract_crossings(&|c| cell_domain[c.0 as usize].map(|d| d.0));
    let groups = graph.group_crossings(&raw_edges);
    let raw_crossings = raw_edges.len();
    let grouped_crossings = groups.len();
    let compression_ratio = if grouped_crossings > 0 {
        raw_crossings as f64 / grouped_crossings as f64
    } else {
        1.0
    };

    let mut ctx = Stage2Ctx {
        graph,
        resolved: &resolved,
        sat: &mut sat,
        cell_domain,
        tol_uv: opts.tol_uv,
        top_ss,
    };

    let mut sink = ViolationSink::new();
    for group in &groups {
        let iso_cuts = collect_strategy_slice_cuts(graph, &resolved, group, true);
        for sub in split_crossing_group_by_cuts(graph, group, &iso_cuts) {
            iso::check_crossing(&mut ctx, &sub, &mut sink);
        }
        let ls_cuts = collect_strategy_slice_cuts(graph, &resolved, group, false);
        for sub in split_crossing_group_by_cuts(graph, group, &ls_cuts) {
            ls::check_crossing(&mut ctx, &sub, &iso_cuts, &mut sink);
        }
    }
    ret_psw::check_retention(&mut ctx, &mut sink);
    ret_psw::check_power_switches(&mut ctx, &mut sink);
    ret_psw::check_port_attributes(&mut ctx, &mut sink);

    let stage2_ms = t_s2.elapsed().as_secs_f64() * 1000.0;

    // Combine Stage 1 + Stage 2, avoiding duplicate Stage 1/2 overlapping IDs on identical targets.
    let s1_keys: HashSet<(String, String)> = violations
        .iter()
        .map(|v| (v.rule_id.clone(), v.signal.clone()))
        .collect();
    for v in sink.list {
        if !s1_keys.contains(&(v.rule_id.clone(), v.signal.clone())) {
            violations.push(v);
        }
    }
    for (idx, v) in violations.iter_mut().enumerate() {
        v.id = idx + 1;
    }

    s.lp.pst_consistent = Some(sat.is_state_space_consistent());
    s.lp.logic_constraints = sat.export_logic_constraints();
    s.lp.resolved = Some(resolved);
    s.lp.violations = violations;
    s.lp.stages_run = vec!["upf".to_string(), "rtl".to_string()];
    s.lp.last_stats = Some(LpBenchStats {
        struct_graph_build_ms,
        pst_sat_build_ms,
        stage1_ms,
        stage2_ms,
        cnf_vars,
        cnf_clauses,
        raw_crossings,
        grouped_crossings,
        compression_ratio,
    });
    s.lp.reapply_waivers();
    Ok(())
}

fn assign_cell_domains(
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
) -> Vec<Option<PowerDomainId>> {
    let mut out = vec![None; graph.cells.len()];
    let mut top_domain: Option<PowerDomainId> = None;
    let mut best_top_rank: u8 = 0;
    for pd in &resolved.intent.domains {
        let excludes_root = pd
            .exclude_elements
            .iter()
            .any(|e| e == "/" || (pd.scope.is_empty() && e == "."));
        if excludes_root {
            continue;
        }
        let claims_root_elem = (pd.scope.is_empty() && pd.elements.iter().any(|e| e == "."))
            || pd.elements.iter().any(|e| e == "/");
        let rank = if pd.is_top && !pd.include_scope {
            4
        } else if claims_root_elem {
            3
        } else if pd.is_top || (pd.include_scope && pd.scope.is_empty()) {
            2
        } else if pd.scope.is_empty() && pd.elements.is_empty() {
            1
        } else {
            0
        };
        if rank > best_top_rank {
            best_top_rank = rank;
            top_domain = Some(pd.id);
        }
    }
    let inst_map: HashMap<&str, PowerDomainId> = resolved
        .instance_domain
        .iter()
        .map(|(k, &v)| (k.as_str(), v))
        .collect();

    for cell in &graph.cells {
        if cell.is_top {
            out[cell.id.0 as usize] = top_domain;
            continue;
        }
        if let Some(&pd) = inst_map.get(cell.upf_path.as_str()) {
            out[cell.id.0 as usize] = Some(pd);
            continue;
        }
        // Inherit nearest ancestor domain.
        let mut cur = cell.parent;
        while let Some(pid) = cur {
            if let Some(pd) = out[pid.0 as usize] {
                out[cell.id.0 as usize] = Some(pd);
                break;
            }
            cur = graph.cell(pid).parent;
        }
    }
    out
}

pub(super) fn sv_escaped_upf_variant(path: &str) -> Option<String> {
    if path.contains('$')
        || path.bytes().any(|b| {
            !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'[' | b']' | b'.' | b':'))
        })
    {
        let esc =
            path.split('/')
                .map(|seg| {
                    if seg.bytes().any(|b| {
                        !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'[' | b']' | b'.'))
                    }) {
                        format!("\\{seg} ")
                    } else {
                        seg.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
        Some(esc)
    } else {
        None
    }
}

pub(super) fn pin_full_path(
    graph: &StructuralGraph,
    pid: informal_svlang::struct_graph::PinId,
) -> String {
    let pin = graph.pin(pid);
    let cell_path = &graph.cell(pin.cell).path;
    if cell_path.is_empty() {
        pin.name.clone()
    } else {
        format!("{cell_path}.{}", pin.name)
    }
}

fn collect_strategy_slice_cuts(
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
    group: &CrossingGroup,
    for_iso: bool,
) -> Vec<BitRange> {
    if group.src_ranges.is_empty() {
        return Vec::new();
    }
    let total_src_w: u32 = group.src_ranges.iter().map(|r| r.width()).sum();
    let total_dst_w: u32 = group.dst_ranges.iter().map(|r| r.width()).sum();
    let src_net = graph.net(group.src_base_net);
    let dst_net = graph.net(group.dst_base_net);
    let src_esc = sv_escaped_upf_variant(&src_net.upf_path);
    let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
    let bpin = group.boundary_pin.map(|bp| {
        let pin = graph.pin(bp);
        (
            pin,
            pin_full_path(graph, bp),
            sv_escaped_upf_variant(&pin.upf_path),
        )
    });
    let mut cuts = Vec::new();
    let elems: Box<dyn Iterator<Item = &String> + '_> = if for_iso {
        Box::new(
            resolved
                .intent
                .isolation_strategies
                .iter()
                .flat_map(|s| s.elements.iter().chain(s.exclude_elements.iter())),
        )
    } else {
        Box::new(
            resolved
                .intent
                .level_shifter_strategies
                .iter()
                .flat_map(|s| s.elements.iter().chain(s.exclude_elements.iter())),
        )
    };
    for elem in elems {
        let Some((base, msb, lsb)) = parse_bit_slice_expr(elem) else {
            continue;
        };
        if endpoint_base_matches(
            &base,
            &src_net.upf_path,
            &src_net.full_path,
            src_esc.as_deref(),
        ) {
            if let Some((lo, hi)) =
                sv_slice_to_phys_range(msb, lsb, src_net.width, src_net.packed_bounds)
            {
                cuts.push(BitRange { lo, hi });
            }
        } else if let Some((pin, ref pfull, ref pesc)) = bpin {
            if pin.cell == group.src_cell
                && endpoint_base_matches(&base, &pin.upf_path, pfull, pesc.as_deref())
            {
                if let Some((lo, hi)) =
                    sv_slice_to_phys_range(msb, lsb, pin.width, pin.packed_bounds)
                {
                    cuts.push(BitRange { lo, hi });
                }
            }
        }
        let dst_phys_opt = if endpoint_base_matches(
            &base,
            &dst_net.upf_path,
            &dst_net.full_path,
            dst_esc.as_deref(),
        ) {
            sv_slice_to_phys_range(msb, lsb, dst_net.width, dst_net.packed_bounds)
        } else if let Some((pin, ref pfull, ref pesc)) = bpin {
            if pin.cell == group.dst_cell
                && endpoint_base_matches(&base, &pin.upf_path, pfull, pesc.as_deref())
            {
                sv_slice_to_phys_range(msb, lsb, pin.width, pin.packed_bounds)
            } else {
                None
            }
        } else {
            None
        };
        if let Some((d_lo, d_hi)) = dst_phys_opt {
            let dst_phys = BitRange { lo: d_lo, hi: d_hi };
            if !group.lanes.is_empty() {
                for &(sr, dr) in &group.lanes {
                    if let Some(ov) = dst_phys.intersect(dr) {
                        if sr.width() > 0 && sr.width() == dr.width() {
                            let off_lo = ov.lo - dr.lo;
                            let off_hi = ov.hi - dr.lo;
                            cuts.push(BitRange {
                                lo: sr.lo + off_lo,
                                hi: sr.lo + off_hi,
                            });
                        } else {
                            cuts.push(sr);
                        }
                    }
                }
            } else if total_src_w > 0 && total_dst_w == total_src_w {
                let mut dst_cum = 0u32;
                for &dr in &group.dst_ranges {
                    if let Some(ov) = dst_phys.intersect(dr) {
                        let off_lo = dst_cum + (ov.lo - dr.lo);
                        let off_hi = dst_cum + (ov.hi - dr.lo);
                        cuts.extend(map_range_list_offset(&group.src_ranges, off_lo, off_hi));
                    }
                    dst_cum += dr.width();
                }
            }
        }
    }
    cuts
}

pub(crate) fn resolve_upf_signal_range_in_scope(
    ctx: &Stage2Ctx<'_>,
    scope: &str,
    sig: &str,
) -> Option<(NetSegId, BitRange)> {
    let trimmed = sig.trim();
    if trimmed.is_empty() {
        return None;
    }
    let try_lookup = |cand: &str| -> Option<(NetSegId, BitRange)> {
        if let Some(nid) = ctx.graph.lookup_net_by_upf(cand) {
            let w = ctx.graph.net(nid).width;
            return Some((nid, BitRange::full(w)));
        }
        if let Some((base, msb, lsb)) = parse_bit_slice_expr(cand) {
            if let Some(nid) = ctx.graph.lookup_net_by_upf(&base) {
                let seg = ctx.graph.net(nid);
                if let Some((lo, hi)) =
                    sv_slice_to_phys_range(msb, lsb, seg.width, seg.packed_bounds)
                {
                    return Some((nid, BitRange { lo, hi }));
                }
            }
        }
        None
    };

    let norm_scope = scope.trim_matches('/');
    if trimmed.starts_with('/') || norm_scope.is_empty() {
        return try_lookup(trimmed);
    }
    if trimmed.starts_with("../") || trimmed.starts_with("./") || trimmed == ".." || trimmed == "."
    {
        let resolved = informal_upf::qualify_signal_ref(norm_scope, trimmed, '/');
        return try_lookup(&resolved);
    }

    let scope_prefix = format!("{norm_scope}/");
    if let Some(tail) = trimmed.strip_prefix(&scope_prefix) {
        if let Some(res) = try_lookup(trimmed) {
            return Some(res);
        }
        let mut cur = norm_scope;
        while let Some((parent, _)) = cur.rsplit_once('/') {
            let cand = format!("{parent}/{tail}");
            if let Some(res) = try_lookup(&cand) {
                return Some(res);
            }
            cur = parent;
        }
        return try_lookup(tail);
    }

    let local_cand = format!("{norm_scope}/{trimmed}");
    if let Some(res) = try_lookup(&local_cand) {
        return Some(res);
    }
    let mut cur = norm_scope;
    while let Some((parent, _)) = cur.rsplit_once('/') {
        let cand = format!("{parent}/{trimmed}");
        if let Some(res) = try_lookup(&cand) {
            return Some(res);
        }
        cur = parent;
    }
    try_lookup(trimmed)
}
