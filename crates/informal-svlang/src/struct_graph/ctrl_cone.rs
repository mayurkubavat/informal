//! Backward control-cone tracer with a 4-valued parity lattice for isolation
//! enables (`iso_en`), retention `save`/`restore`, and power-switch controls.

use std::collections::{BTreeSet, HashMap, VecDeque};

use super::{BitRange, CellId, DriverOrigin, ExprOpSummary, NetSegId, NetSegKind, StructuralGraph};

/// 4-valued parity lattice along backward combinational control cones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Polarity {
    /// Strictly non-inverting path(s) from root driver.
    Positive,
    /// Odd number of inversions on all path(s) from root driver.
    Negative,
    /// Reconvergent paths from the same root with BOTH Positive and Negative parity.
    Mixed,
    /// Non-unate (`xor`, arithmetic, dynamic index, general mux, blackbox, unresolved).
    Unknown,
}

impl Polarity {
    /// Lattice join at reconvergent paths.
    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (a, b) if a == b => a,
            (Self::Mixed, _) | (_, Self::Mixed) => Self::Mixed,
            (Self::Positive, Self::Negative) | (Self::Negative, Self::Positive) => Self::Mixed,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Unknown,
        }
    }

    /// Invert parity (`Positive <-> Negative`, preserving `Mixed` and `Unknown`).
    pub fn invert(self) -> Self {
        match self {
            Self::Positive => Self::Negative,
            Self::Negative => Self::Positive,
            Self::Mixed => Self::Mixed,
            Self::Unknown => Self::Unknown,
        }
    }

    /// Compose accumulated parity across a combinational edge op.
    pub fn compose(self, op: &ExprOpSummary) -> Self {
        match op {
            ExprOpSummary::Identity => self,
            ExprOpSummary::Invert => self.invert(),
            ExprOpSummary::UnateGate { is_inverting } => {
                if *is_inverting {
                    self.invert()
                } else {
                    self
                }
            }
            ExprOpSummary::Mux { sel_const_true, .. } => match sel_const_true {
                Some(true) => self,
                Some(false) => self.invert(),
                None => Self::Unknown,
            },
            ExprOpSummary::Const { .. } | ExprOpSummary::Comb | ExprOpSummary::Seq => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PolarityAcc {
    has_pos: bool,
    has_neg: bool,
    has_unk: bool,
}

impl PolarityAcc {
    fn from_polarity(p: Polarity) -> Self {
        match p {
            Polarity::Positive => Self {
                has_pos: true,
                has_neg: false,
                has_unk: false,
            },
            Polarity::Negative => Self {
                has_pos: false,
                has_neg: true,
                has_unk: false,
            },
            Polarity::Mixed => Self {
                has_pos: true,
                has_neg: true,
                has_unk: false,
            },
            Polarity::Unknown => Self {
                has_pos: false,
                has_neg: false,
                has_unk: true,
            },
        }
    }

    fn merge(&mut self, p: Polarity) {
        let other = Self::from_polarity(p);
        self.has_pos |= other.has_pos;
        self.has_neg |= other.has_neg;
        self.has_unk |= other.has_unk;
    }

    fn collapse(self) -> Polarity {
        if self.has_pos && self.has_neg {
            Polarity::Mixed
        } else if self.has_unk {
            Polarity::Unknown
        } else if self.has_pos {
            Polarity::Positive
        } else if self.has_neg {
            Polarity::Negative
        } else {
            Polarity::Unknown
        }
    }
}

#[derive(Debug, Clone)]
pub struct CtrlRoot {
    pub net: NetSegId,
    pub range: BitRange,
    pub cell: CellId,
    pub polarity: Polarity,
    pub is_seq: bool,
    pub is_const: bool,
    pub is_primary_input: bool,
    pub is_blackbox: bool,
    pub hops: usize,
}

#[derive(Debug, Clone)]
pub struct CtrlCone {
    pub start_net: NetSegId,
    pub overall_polarity: Polarity,
    pub roots: Vec<CtrlRoot>,
    pub cone_cells: Vec<CellId>,
    pub cone_nets: Vec<NetSegId>,
    pub net_polarities: HashMap<NetSegId, Polarity>,
    pub net_ranges: HashMap<NetSegId, Vec<BitRange>>,
}

impl CtrlCone {
    /// Returns the parity from `target_net` to `start_net`, if `target_net` is in the cone.
    pub fn polarity_from(&self, target_net: NetSegId) -> Option<Polarity> {
        self.net_polarities.get(&target_net).copied()
    }
}

/// Traces backward along drivers from `(start_net, start_range)` across hierarchy
/// boundaries, accumulating 4-valued parity lattice labels.
pub fn trace_ctrl_cone(
    graph: &StructuralGraph,
    start_net: NetSegId,
    start_range: BitRange,
    max_depth: usize,
) -> CtrlCone {
    let mut net_acc: HashMap<NetSegId, PolarityAcc> = HashMap::new();
    let mut net_ranges: HashMap<NetSegId, Vec<BitRange>> = HashMap::new();
    let mut visited: HashMap<(NetSegId, BitRange, Polarity), usize> = HashMap::new();
    let mut queue: VecDeque<(NetSegId, BitRange, Polarity, usize)> = VecDeque::new();
    let mut cone_cells_set: BTreeSet<CellId> = BTreeSet::new();
    let mut cone_nets_set: BTreeSet<NetSegId> = BTreeSet::new();
    let mut root_map: HashMap<NetSegId, CtrlRoot> = HashMap::new();

    net_acc
        .entry(start_net)
        .or_default()
        .merge(Polarity::Positive);
    record_net_range(&mut net_ranges, start_net, start_range);
    visited.insert((start_net, start_range, Polarity::Positive), 0);
    queue.push_back((start_net, start_range, Polarity::Positive, 0));

    while let Some((cur_net, cur_range, cur_pol, depth)) = queue.pop_front() {
        let seg = graph.net(cur_net);
        cone_cells_set.insert(seg.cell);
        cone_nets_set.insert(cur_net);

        let is_supply = matches!(seg.kind, NetSegKind::Supply0 | NetSegKind::Supply1);
        let is_blackbox = graph.cell(seg.cell).is_blackbox;
        if seg.is_seq_state
            || is_supply
            || is_blackbox
            || seg.drivers.is_empty()
            || depth >= max_depth
        {
            record_root(
                &mut root_map,
                cur_net,
                cur_range,
                seg.cell,
                cur_pol,
                seg.is_seq_state,
                is_supply,
                false,
                is_blackbox,
                depth,
            );
            continue;
        }

        let mut pushed_any = false;
        for drv in &seg.drivers {
            if !drv.dst_range.overlaps(cur_range) {
                continue;
            }
            match &drv.origin {
                DriverOrigin::PrimaryInput(_) => {
                    record_root(
                        &mut root_map,
                        cur_net,
                        cur_range,
                        seg.cell,
                        cur_pol,
                        false,
                        false,
                        true,
                        false,
                        depth,
                    );
                    continue;
                }
                DriverOrigin::Supply => {
                    record_root(
                        &mut root_map,
                        cur_net,
                        cur_range,
                        seg.cell,
                        cur_pol,
                        false,
                        true,
                        false,
                        false,
                        depth,
                    );
                    continue;
                }
                DriverOrigin::AlwaysFf | DriverOrigin::AlwaysLatch => {
                    record_root(
                        &mut root_map,
                        cur_net,
                        cur_range,
                        seg.cell,
                        cur_pol,
                        true,
                        false,
                        false,
                        false,
                        depth,
                    );
                    continue;
                }
                DriverOrigin::Blackbox => {
                    record_root(
                        &mut root_map,
                        cur_net,
                        cur_range,
                        seg.cell,
                        Polarity::Unknown,
                        false,
                        false,
                        false,
                        true,
                        depth,
                    );
                    continue;
                }
                _ => {}
            }

            if matches!(drv.op, ExprOpSummary::Const { .. }) && drv.src_net.is_none() {
                record_root(
                    &mut root_map,
                    cur_net,
                    cur_range,
                    seg.cell,
                    cur_pol,
                    false,
                    true,
                    false,
                    false,
                    depth,
                );
                continue;
            }

            if let ExprOpSummary::Mux {
                sel: Some((sel_net, sel_range)),
                sel_const_true: Some(const_t),
            } = drv.op
            {
                let edge_pol = if const_t { cur_pol } else { cur_pol.invert() };
                enqueue(
                    &mut net_acc,
                    &mut net_ranges,
                    &mut visited,
                    &mut queue,
                    sel_net,
                    sel_range,
                    edge_pol,
                    depth + 1,
                );
                pushed_any = true;
                continue;
            }

            let next_pol = match &drv.op {
                ExprOpSummary::Mux {
                    sel_const_true: None,
                    ..
                } => {
                    // For a general mux `sel ? a : b`, push `a`, `b` with `cur_pol`,
                    // and `sel` with `Unknown` (non-unate select).
                    if let ExprOpSummary::Mux {
                        sel: Some((sel_net, sel_range)),
                        ..
                    } = drv.op
                    {
                        enqueue(
                            &mut net_acc,
                            &mut net_ranges,
                            &mut visited,
                            &mut queue,
                            sel_net,
                            sel_range,
                            Polarity::Unknown,
                            depth + 1,
                        );
                        pushed_any = true;
                    }
                    cur_pol
                }
                other => cur_pol.compose(other),
            };

            if let Some(sn) = drv.src_net {
                let sr = drv
                    .src_range
                    .unwrap_or_else(|| BitRange::full(graph.net(sn).width));
                let next_range =
                    if matches!(drv.op, ExprOpSummary::Identity | ExprOpSummary::Invert)
                        && drv.dst_range.width() == sr.width()
                    {
                        if let Some(ov) = drv.dst_range.intersect(cur_range) {
                            let lo_off = ov.lo - drv.dst_range.lo;
                            let hi_off = ov.hi - drv.dst_range.lo;
                            BitRange {
                                lo: sr.lo + lo_off,
                                hi: sr.lo + hi_off,
                            }
                        } else {
                            sr
                        }
                    } else {
                        sr
                    };
                enqueue(
                    &mut net_acc,
                    &mut net_ranges,
                    &mut visited,
                    &mut queue,
                    sn,
                    next_range,
                    next_pol,
                    depth + 1,
                );
                pushed_any = true;
            }
            for (idx, &(dep_net, dep_range)) in drv.extra_deps.iter().enumerate() {
                if drv.src_net == Some(dep_net) && drv.src_range == Some(dep_range) {
                    continue;
                }
                let dep_pol = drv
                    .dep_ops
                    .get(idx)
                    .map(|op| cur_pol.compose(op))
                    .unwrap_or(next_pol);
                enqueue(
                    &mut net_acc,
                    &mut net_ranges,
                    &mut visited,
                    &mut queue,
                    dep_net,
                    dep_range,
                    dep_pol,
                    depth + 1,
                );
                pushed_any = true;
            }
        }

        if !pushed_any && !root_map.contains_key(&cur_net) {
            record_root(
                &mut root_map,
                cur_net,
                cur_range,
                seg.cell,
                cur_pol,
                seg.is_seq_state,
                is_supply,
                false,
                is_blackbox,
                depth,
            );
        }
    }

    let net_polarities: HashMap<NetSegId, Polarity> = net_acc
        .into_iter()
        .map(|(n, acc)| (n, acc.collapse()))
        .collect();

    let mut roots: Vec<CtrlRoot> = root_map.into_values().collect();
    roots.sort_by_key(|r| (r.hops, r.net.0));
    for r in &mut roots {
        if let Some(&pol) = net_polarities.get(&r.net) {
            r.polarity = pol;
        }
    }
    let has_mixed = roots.iter().any(|r| r.polarity == Polarity::Mixed)
        || net_polarities.values().any(|&p| p == Polarity::Mixed);
    let overall_polarity = if has_mixed {
        Polarity::Mixed
    } else if roots.is_empty() {
        Polarity::Positive
    } else {
        let first = roots[0].polarity;
        if roots.iter().all(|r| r.polarity == first) {
            first
        } else {
            Polarity::Unknown
        }
    };

    CtrlCone {
        start_net,
        overall_polarity,
        roots,
        cone_cells: cone_cells_set.into_iter().collect(),
        cone_nets: cone_nets_set.into_iter().collect(),
        net_polarities,
        net_ranges,
    }
}

fn record_net_range(
    net_ranges: &mut HashMap<NetSegId, Vec<BitRange>>,
    net: NetSegId,
    range: BitRange,
) {
    let list = net_ranges.entry(net).or_default();
    if !list.contains(&range) {
        list.push(range);
    }
}

#[allow(clippy::too_many_arguments)]
fn enqueue(
    net_acc: &mut HashMap<NetSegId, PolarityAcc>,
    net_ranges: &mut HashMap<NetSegId, Vec<BitRange>>,
    visited: &mut HashMap<(NetSegId, BitRange, Polarity), usize>,
    queue: &mut VecDeque<(NetSegId, BitRange, Polarity, usize)>,
    net: NetSegId,
    range: BitRange,
    pol: Polarity,
    depth: usize,
) {
    net_acc.entry(net).or_default().merge(pol);
    record_net_range(net_ranges, net, range);
    let prev = visited.get(&(net, range, pol)).copied();
    if prev.is_none_or(|d| depth < d) {
        visited.insert((net, range, pol), depth);
        queue.push_back((net, range, pol, depth));
    }
}

#[allow(clippy::too_many_arguments)]
fn record_root(
    root_map: &mut HashMap<NetSegId, CtrlRoot>,
    net: NetSegId,
    range: BitRange,
    cell: CellId,
    polarity: Polarity,
    is_seq: bool,
    is_const: bool,
    is_primary_input: bool,
    is_blackbox: bool,
    hops: usize,
) {
    root_map
        .entry(net)
        .and_modify(|e| {
            e.polarity = e.polarity.join(polarity);
            e.is_seq |= is_seq;
            e.is_const |= is_const;
            e.is_primary_input |= is_primary_input;
            e.is_blackbox |= is_blackbox;
            e.hops = e.hops.min(hops);
        })
        .or_insert(CtrlRoot {
            net,
            range,
            cell,
            polarity,
            is_seq,
            is_const,
            is_primary_input,
            is_blackbox,
            hops,
        });
}
