//! Hierarchical pin-net structural graph built directly from
//! `svlang::ElaboratedDesign` + `svlang::ExprBinder` without flattening instance
//! boundaries or requiring `sv-netlist` lowering.

pub mod builder;
pub mod bus_group;
pub mod ctrl_cone;
mod expr_summary;
pub(crate) mod expr_walk;
pub mod feedthrough;
mod ft_alias;
mod ft_classify;
pub(crate) mod stmt_clamp;
pub(crate) mod stmt_walk;

use std::collections::HashMap;

pub use bus_group::{
    group_crossings, map_range_list_offset, split_crossing_group_by_cuts, CrossingGroup,
};
pub use ctrl_cone::{trace_ctrl_cone, CtrlCone, CtrlRoot, Polarity};
pub use feedthrough::{extract_crossings, CrossingEdge, FeedthroughKind, FeedthroughRoute};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetSegId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(pub u32);

/// Inclusive 0-based physical bit range `[lo ..= hi]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BitRange {
    pub lo: u32,
    pub hi: u32,
}

impl BitRange {
    pub fn full(width: u32) -> Self {
        Self {
            lo: 0,
            hi: width.saturating_sub(1),
        }
    }

    pub fn width(self) -> u32 {
        if self.hi >= self.lo {
            self.hi - self.lo + 1
        } else {
            0
        }
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.lo <= other.hi && other.lo <= self.hi
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        self.overlaps(other).then(|| Self {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
        })
    }
}

/// Inline RTL isolation clamp gate recognized on a continuous assignment or comb mux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InlineClampInfo {
    pub ctrl_net: NetSegId,
    pub ctrl_range: BitRange,
    /// `true` if `ctrl == 1` clamps output; `false` if `ctrl == 0` clamps output.
    pub active_high: bool,
    /// `false` if clamped to `0`; `true` if clamped to `1`.
    pub clamp_one: bool,
    pub data_net: NetSegId,
    pub data_range: BitRange,
}

/// Lightweight operator classification for feedthrough and control-cone polarity analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprOpSummary {
    Identity,
    Invert,
    Const {
        is_zero: bool,
        is_all_ones: bool,
    },
    UnateGate {
        is_inverting: bool,
    },
    Mux {
        sel: Option<(NetSegId, BitRange)>,
        sel_const_true: Option<bool>,
    },
    Comb,
    Seq,
}

/// Origin of a driver edge onto a [`NetSegment`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverOrigin {
    PrimaryInput(PinId),
    ParentPort(PinId),
    ChildPin(PinId),
    Assign,
    AlwaysComb,
    AlwaysFf,
    AlwaysLatch,
    Gate(String),
    Udp(String),
    NetAlias,
    Supply,
    Blackbox,
}

#[derive(Debug, Clone)]
pub struct DriverEdge {
    pub dst_range: BitRange,
    pub src_net: Option<NetSegId>,
    pub src_range: Option<BitRange>,
    pub origin: DriverOrigin,
    pub op: ExprOpSummary,
    pub extra_deps: Vec<(NetSegId, BitRange)>,
    pub dep_ops: Vec<ExprOpSummary>,
    pub inline_clamp: Option<InlineClampInfo>,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone)]
pub struct LoadEdge {
    pub src_range: BitRange,
    pub dst_net: Option<NetSegId>,
    pub dst_pin: Option<PinId>,
    pub dst_range: BitRange,
    pub op: ExprOpSummary,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone)]
pub struct PinConnBinding {
    pub pin_range: BitRange,
    pub parent_net: Option<NetSegId>,
    pub parent_range: BitRange,
    pub expr_op: ExprOpSummary,
    pub expr_deps: Vec<(NetSegId, BitRange)>,
    pub dep_ops: Vec<ExprOpSummary>,
    pub inline_clamp: Option<InlineClampInfo>,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone)]
pub struct SeqElement {
    pub net: NetSegId,
    pub range: BitRange,
    pub full_path: String,
    pub upf_path: String,
    pub is_latch: bool,
    pub clock: Option<NetSegId>,
    pub async_reset: Option<NetSegId>,
    pub async_reset_range: Option<BitRange>,
    pub async_reset_active_high: Option<bool>,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone)]
pub struct CellNode {
    pub id: CellId,
    pub inst_id: Option<svlang::InstanceId>,
    pub parent: Option<CellId>,
    pub children: Vec<CellId>,
    pub name: String,
    pub raw_name: String,
    pub path: String,
    pub upf_path: String,
    pub module: String,
    pub is_top: bool,
    pub is_interface: bool,
    pub is_checker: bool,
    pub is_blackbox: bool,
    pub is_bound: bool,
    pub pins: Vec<PinId>,
    pub nets: Vec<NetSegId>,
    pub flops: Vec<SeqElement>,
    pub latches: Vec<SeqElement>,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone)]
pub struct PinNode {
    pub id: PinId,
    pub cell: CellId,
    pub symbol: Option<svlang::SymbolId>,
    pub name: String,
    pub upf_path: String,
    pub width: u32,
    pub packed_bounds: Option<(i128, i128)>,
    pub dir: Option<svlang::PortDir>,
    pub internal_net: Option<NetSegId>,
    pub external_conns: Vec<PinConnBinding>,
    pub unconnected: bool,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetSegKind {
    Port(svlang::PortDir),
    Wire,
    Var,
    Reg,
    Latch,
    Supply0,
    Supply1,
    Memory,
    BlackboxStub,
}

#[derive(Debug, Clone)]
pub struct NetSegment {
    pub id: NetSegId,
    pub cell: CellId,
    pub symbol: Option<svlang::SymbolId>,
    pub name: String,
    pub full_path: String,
    pub upf_path: String,
    pub width: u32,
    pub packed_bounds: Option<(i128, i128)>,
    pub kind: NetSegKind,
    pub is_seq_state: bool,
    pub drivers: Vec<DriverEdge>,
    pub loads: Vec<LoadEdge>,
    pub loc: svlang::Loc,
}

/// Arena-backed hierarchical pin-net structural graph built from `ElaboratedDesign`.
#[derive(Debug, Clone)]
pub struct StructuralGraph {
    pub files: Vec<String>,
    pub top_names: Vec<String>,
    pub tops: Vec<CellId>,
    pub cells: Vec<CellNode>,
    pub pins: Vec<PinNode>,
    pub nets: Vec<NetSegment>,
    pub cell_by_path: HashMap<String, CellId>,
    pub cell_by_upf: HashMap<String, CellId>,
    pub net_by_path: HashMap<String, NetSegId>,
    pub net_by_upf: HashMap<String, NetSegId>,
    pub sym_to_net: HashMap<svlang::SymbolId, NetSegId>,
    pub pin_by_upf: HashMap<String, PinId>,
    pub inst_to_cell: HashMap<svlang::InstanceId, CellId>,
}

impl StructuralGraph {
    /// Builds a hierarchical [`StructuralGraph`] directly from `ElaboratedDesign`.
    pub fn build(
        trees: &[svlang::Tree],
        design: &svlang::ElaboratedDesign,
        top: Option<&str>,
    ) -> Self {
        builder::build_structural_graph(trees, design, top)
    }

    pub fn cell(&self, id: CellId) -> &CellNode {
        &self.cells[id.0 as usize]
    }

    pub fn pin(&self, id: PinId) -> &PinNode {
        &self.pins[id.0 as usize]
    }

    pub fn net(&self, id: NetSegId) -> &NetSegment {
        &self.nets[id.0 as usize]
    }

    /// Formats a `Loc` as `"file:line"`.
    pub fn format_loc(&self, loc: svlang::Loc) -> Option<String> {
        let file = self.files.get(loc.file as usize)?;
        if file.is_empty() || loc.line == 0 {
            None
        } else {
            Some(format!("{file}:{}", loc.line))
        }
    }

    /// Looks up a cell by UPF path (`/` or `.` separated, with optional leading `/` or top name).
    pub fn lookup_cell_by_upf(&self, raw: &str) -> Option<CellId> {
        let norm = normalize_query_path(raw);
        self.cell_by_upf
            .get(&norm)
            .or_else(|| self.cell_by_path.get(&norm))
            .copied()
            .or_else(|| {
                for t in &self.top_names {
                    let prefixed = if norm.is_empty() {
                        t.clone()
                    } else {
                        format!("{t}/{norm}")
                    };
                    if let Some(&id) = self.cell_by_upf.get(&prefixed) {
                        return Some(id);
                    }
                    if let Some(stripped) = norm.strip_prefix(&format!("{t}/")) {
                        if let Some(&id) = self.cell_by_upf.get(stripped) {
                            return Some(id);
                        }
                    }
                }
                None
            })
    }

    /// Looks up a net segment by UPF or SV path.
    pub fn lookup_net_by_upf(&self, raw: &str) -> Option<NetSegId> {
        let norm = normalize_query_path(raw);
        self.net_by_upf
            .get(&norm)
            .or_else(|| self.net_by_path.get(&norm))
            .copied()
            .or_else(|| {
                for t in &self.top_names {
                    let prefixed = format!("{t}/{norm}");
                    if let Some(&id) = self.net_by_upf.get(&prefixed) {
                        return Some(id);
                    }
                    if let Some(stripped) = norm.strip_prefix(&format!("{t}/")) {
                        if let Some(&id) = self.net_by_upf.get(stripped) {
                            return Some(id);
                        }
                    }
                }
                None
            })
    }

    /// Looks up a pin by UPF path.
    pub fn lookup_pin_by_upf(&self, raw: &str) -> Option<PinId> {
        let norm = normalize_query_path(raw);
        self.pin_by_upf.get(&norm).copied().or_else(|| {
            for t in &self.top_names {
                if let Some(stripped) = norm.strip_prefix(&format!("{t}/")) {
                    if let Some(&id) = self.pin_by_upf.get(stripped) {
                        return Some(id);
                    }
                }
            }
            None
        })
    }

    /// Extracts raw domain-crossing edges using `domain_of(cell_id)`.
    pub fn extract_crossings(
        &self,
        domain_of: &dyn Fn(CellId) -> Option<u32>,
    ) -> Vec<CrossingEdge> {
        feedthrough::extract_crossings(self, domain_of)
    }

    /// Coalesces bit/slice crossing edges into bus-level [`CrossingGroup`]s.
    pub fn group_crossings(&self, edges: &[CrossingEdge]) -> Vec<CrossingGroup> {
        bus_group::group_crossings(self, edges)
    }

    /// Traces backward control cone from `(start_net, start_range)` with 4-valued parity lattice.
    pub fn trace_ctrl_cone(
        &self,
        start_net: NetSegId,
        start_range: BitRange,
        max_depth: usize,
    ) -> CtrlCone {
        ctrl_cone::trace_ctrl_cone(self, start_net, start_range, max_depth)
    }

    /// Returns `(net_id, net_range, route_dst_subrange)` for `start_net` and any pre-combinational
    /// pure-wire alias nets reachable before any combinational/buffered hop on a feedthrough route.
    pub fn pre_comb_nets_for_route(
        &self,
        start_net: NetSegId,
        start_range: BitRange,
        reaching_nets: &[(NetSegId, BitRange)],
    ) -> Vec<(NetSegId, BitRange, BitRange)> {
        ft_alias::collect_pre_comb_nets(self, start_net, start_range, reaching_nets)
    }
}

/// Normalizes a single SystemVerilog leaf identifier (stripping leading `\` and
/// collapsing whitespace before trailing `[...]` array/bit indices).
pub fn normalize_sv_leaf(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('\\') {
        if let Some(brk) = rest.rfind('[') {
            let head = rest[..brk].trim_end();
            let tail = &rest[brk..];
            format!("{head}{tail}")
        } else {
            rest.trim_end().to_string()
        }
    } else {
        trimmed.to_string()
    }
}

/// Splits a hierarchical path on `/` or `.` while preserving SV escaped tokens `\... `.
pub fn normalize_query_path(raw: &str) -> String {
    let s = raw.trim().trim_matches('/');
    if s.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'/' || bytes[i] == b'.' {
            i += 1;
            continue;
        }
        if bytes[i] == b'\\' {
            let start = i;
            i += 1;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Include optional trailing `[idx]` after whitespace if separated by space.
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            let slice_end = if end < bytes.len() && bytes[end] == b'[' {
                if let Some(close) = s[end..].find(']') {
                    i = end + close + 1;
                    i
                } else {
                    i = end;
                    i
                }
            } else {
                i = end;
                i
            };
            let leaf = normalize_sv_leaf(&s[start..slice_end]);
            if !leaf.is_empty() {
                parts.push(leaf);
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b'/' && bytes[i] != b'.' {
                i += 1;
            }
            let leaf = normalize_sv_leaf(&s[start..i]);
            if !leaf.is_empty() {
                parts.push(leaf);
            }
        }
    }
    parts.join("/")
}

/// Formats a physical bit range using declared `packed_bounds` if present.
pub fn format_bit_range(
    base: &str,
    width: u32,
    packed_bounds: Option<(i128, i128)>,
    range: BitRange,
) -> String {
    if width <= 1 || (range.lo == 0 && range.hi + 1 >= width) {
        return base.to_string();
    }
    let (lo_sv, hi_sv) = match packed_bounds {
        Some((left, right)) => {
            if left >= right {
                (right + range.lo as i128, right + range.hi as i128)
            } else {
                (right - range.lo as i128, right - range.hi as i128)
            }
        }
        None => (range.lo as i128, range.hi as i128),
    };
    if range.lo == range.hi {
        format!("{base}[{lo_sv}]")
    } else {
        format!("{base}[{hi_sv}:{lo_sv}]")
    }
}
