//! Fast structural elaborator: lowers parsed structural Verilog modules +
//! `LibertyDb` directly into an arena-backed `StructuralGraph` and lightweight
//! `Design` summary for 100k+ cell gate/PG netlists.

use std::collections::{HashMap, HashSet};

use informal_liberty::{LibertyCell, LibertyDb, PgType, PinDir};
use svlang::semantic::PortDir;

use super::parser::{ParsedExpr, ParsedInst, ParsedModule};
use crate::struct_graph::{
    BitRange, CellId, CellNode, DriverEdge, DriverOrigin, ExprOpSummary, NetSegId, NetSegKind,
    NetSegment, PinId, PinNode, SeqElement, StructuralGraph,
};
use crate::types::{Design, Instance, Scope, ScopeSignal, Signal};

pub(super) fn make_loc(file: u32, line: u32) -> svlang::Loc {
    svlang::Loc {
        file,
        line,
        column: 1,
        expansion: None,
    }
}

pub struct FastElabResult {
    pub top: String,
    pub graph: StructuralGraph,
    pub design: Design,
}

pub fn elaborate_structural(
    files: Vec<String>,
    modules: &[ParsedModule],
    top_opt: Option<&str>,
    liberty: &LibertyDb,
) -> Result<FastElabResult, String> {
    let mod_map: HashMap<&str, &ParsedModule> =
        modules.iter().map(|m| (m.name.as_str(), m)).collect();

    // Infer top module(s): defined modules never instantiated by another module
    // (excluding modules that are Liberty stdcells when non-stdcell modules exist).
    let mut instantiated: HashSet<&str> = HashSet::new();
    for m in modules {
        for inst in &m.insts {
            instantiated.insert(inst.module_name.as_str());
        }
    }
    let tops: Vec<String> = modules
        .iter()
        .filter(|m| !instantiated.contains(m.name.as_str()) && !liberty.contains_cell(&m.name))
        .map(|m| m.name.clone())
        .collect();

    let top_name = match top_opt {
        Some(t) => {
            if !mod_map.contains_key(t) {
                return Err(format!("gate_fast: top module `{t}` not found"));
            }
            t.to_string()
        }
        None => match tops.as_slice() {
            [only] => only.clone(),
            _ => {
                return Err(format!(
                    "gate_fast: cannot infer unique top module (candidates: {tops:?}); pass -top"
                ));
            }
        },
    };

    // Pre-estimate total cells/nets/pins so 100k-cell elaboration avoids reallocating.
    let est_cells = modules.iter().map(|m| m.insts.len()).sum::<usize>().max(16) + 8;
    let est_nets = modules.iter().map(|m| m.nets.len()).sum::<usize>().max(16) + 8;
    let est_pins = est_cells.saturating_mul(4);

    let mut graph = StructuralGraph {
        files,
        top_names: vec![top_name.clone()],
        tops: Vec::with_capacity(1),
        cells: Vec::with_capacity(est_cells),
        pins: Vec::with_capacity(est_pins),
        nets: Vec::with_capacity(est_nets),
        cell_by_path: HashMap::with_capacity(est_cells),
        cell_by_upf: HashMap::with_capacity(est_cells),
        net_by_path: HashMap::with_capacity(est_nets),
        net_by_upf: HashMap::with_capacity(est_nets),
        sym_to_net: HashMap::new(),
        pin_by_upf: HashMap::with_capacity(1024),
        inst_to_cell: HashMap::new(),
    };

    let mut design = Design {
        ok: true,
        tops: vec![top_name.clone()],
        ..Design::default()
    };

    let mut elab = ElabCtx {
        mod_map: &mod_map,
        liberty,
        graph: &mut graph,
        instances: Vec::with_capacity(64),
        signals: Vec::with_capacity(256),
    };

    let top_mod = mod_map[top_name.as_str()];
    let (top_cid, top_scope) = elab.instantiate_module(
        top_mod,
        None,
        &top_name,
        &top_name,
        &top_name,
        "",
        true,
    );
    elab.graph.tops.push(top_cid);

    design.instances = elab.instances;
    design.signals = elab.signals;
    design.hierarchy = vec![top_scope];

    Ok(FastElabResult {
        top: top_name,
        graph,
        design,
    })
}

pub(super) struct ElabCtx<'a> {
    pub(super) mod_map: &'a HashMap<&'a str, &'a ParsedModule>,
    pub(super) liberty: &'a LibertyDb,
    pub(super) graph: &'a mut StructuralGraph,
    pub(super) instances: Vec<Instance>,
    pub(super) signals: Vec<Signal>,
}

impl<'a> ElabCtx<'a> {
    #[allow(clippy::too_many_arguments)]
    fn instantiate_module(
        &mut self,
        m: &ParsedModule,
        parent: Option<CellId>,
        inst_name: &str,
        raw_name: &str,
        dot_path: &str,
        upf_path: &str,
        is_top: bool,
    ) -> (CellId, Scope) {
        let cid = CellId(self.graph.cells.len() as u32);
        let loc = make_loc(m.file_idx, m.line);
        self.graph.cells.push(CellNode {
            id: cid,
            inst_id: None,
            parent,
            children: Vec::new(),
            name: inst_name.to_string(),
            raw_name: raw_name.to_string(),
            path: dot_path.to_string(),
            upf_path: upf_path.to_string(),
            module: m.name.clone(),
            is_top,
            is_interface: false,
            is_checker: false,
            is_blackbox: false,
            is_bound: false,
            pins: Vec::with_capacity(m.ports.len()),
            nets: Vec::with_capacity(m.nets.len()),
            flops: Vec::new(),
            latches: Vec::new(),
            loc,
        });
        if let Some(p) = parent {
            self.graph.cells[p.0 as usize].children.push(cid);
        }
        self.graph.cell_by_path.insert(dot_path.to_string(), cid);
        if !upf_path.is_empty() {
            self.graph.cell_by_upf.insert(upf_path.to_string(), cid);
        }

        self.instances.push(Instance {
            path: dot_path.to_string(),
            module: m.name.clone(),
        });

        let mut local_nets: HashMap<String, NetSegId> = HashMap::with_capacity(m.nets.len() + 8);
        let mut scope_ports: Vec<ScopeSignal> = Vec::new();
        let mut scope_signals: Vec<ScopeSignal> = Vec::new();

        // 1. Allocate nets for all module ports & wires.
        for pn in &m.nets {
            if local_nets.contains_key(&pn.name) {
                continue;
            }
            let nid = self.alloc_net(
                cid,
                dot_path,
                upf_path,
                &pn.name,
                pn.width,
                pn.packed_bounds,
                pn.kind,
                make_loc(m.file_idx, pn.line),
            );
            local_nets.insert(pn.name.clone(), nid);
            if is_top || m.insts.len() <= 512 {
                self.signals.push(Signal {
                    path: format!("{dot_path}.{}", pn.name),
                    width: pn.width as u64,
                    constant: false,
                });
                if !matches!(pn.kind, NetSegKind::Port(_)) && scope_signals.len() < 256 {
                    scope_signals.push(ScopeSignal {
                        name: pn.name.clone(),
                        width: pn.width as u64,
                        kind: "net".to_string(),
                        dir: None,
                    });
                }
            }
        }

        // 2. Allocate pins for all module ports (linking `internal_net`).
        for p in &m.ports {
            let dir = p.dir;
            let nid = match local_nets.get(&p.name).copied() {
                Some(n) => n,
                None => {
                    let n = self.alloc_net(
                        cid,
                        dot_path,
                        upf_path,
                        &p.name,
                        p.width,
                        p.packed_bounds,
                        dir.map_or(NetSegKind::Wire, NetSegKind::Port),
                        make_loc(m.file_idx, p.line),
                    );
                    local_nets.insert(p.name.clone(), n);
                    n
                }
            };
            let pin_upf = join_upf(upf_path, &p.name);
            let pid = PinId(self.graph.pins.len() as u32);
            let ploc = make_loc(m.file_idx, p.line);
            self.graph.pins.push(PinNode {
                id: pid,
                cell: cid,
                symbol: None,
                name: p.name.clone(),
                upf_path: pin_upf.clone(),
                width: p.width,
                packed_bounds: p.packed_bounds,
                dir,
                internal_net: Some(nid),
                external_conns: Vec::new(),
                unconnected: false,
                loc: ploc,
            });
            self.graph.cells[cid.0 as usize].pins.push(pid);
            self.graph.pin_by_upf.insert(pin_upf, pid);
            if is_top && matches!(dir, Some(PortDir::Input | PortDir::Inout)) {
                self.graph.nets[nid.0 as usize].drivers.push(DriverEdge {
                    dst_range: BitRange::full(p.width),
                    src_net: None,
                    src_range: None,
                    origin: DriverOrigin::PrimaryInput(pid),
                    op: ExprOpSummary::Identity,
                    extra_deps: Vec::new(),
                    dep_ops: Vec::new(),
                    inline_clamp: None,
                    loc: ploc,
                });
            }
            scope_ports.push(ScopeSignal {
                name: p.name.clone(),
                width: p.width as u64,
                kind: "port".to_string(),
                dir: dir.map(|d| match d {
                    PortDir::Input => "input".to_string(),
                    PortDir::Output => "output".to_string(),
                    PortDir::Inout => "inout".to_string(),
                    PortDir::Ref => "ref".to_string(),
                }),
            });
        }

        // 3. Lower continuous assignments (`assign lhs = rhs;`).
        for asgn in &m.assigns {
            let aloc = make_loc(m.file_idx, asgn.line);
            let dst_nid = self.ensure_local_net(
                cid,
                dot_path,
                upf_path,
                &asgn.lhs_net,
                &mut local_nets,
                aloc,
            );
            let dst_w = self.graph.nets[dst_nid.0 as usize].width;
            let dst_range = asgn
                .lhs_range
                .map(|(lo, hi)| BitRange { lo, hi })
                .unwrap_or_else(|| BitRange::full(dst_w));

            self.wire_assign_expr(
                cid,
                dot_path,
                upf_path,
                dst_nid,
                dst_range,
                &asgn.rhs,
                &mut local_nets,
                aloc,
            );
        }

        // 4. Instantiate child modules and Liberty leaf cells.
        let mut child_scopes = Vec::new();
        for inst in &m.insts {
            let child_dot = format!("{dot_path}.{}", inst.inst_name);
            let child_upf = join_upf(upf_path, &inst.inst_name);
            let iloc = make_loc(m.file_idx, inst.line);
            if let Some(lib_cell) = self.liberty.cell(&inst.module_name) {
                self.instantiate_liberty_leaf(
                    cid,
                    dot_path,
                    upf_path,
                    inst,
                    lib_cell,
                    &child_dot,
                    &child_upf,
                    &mut local_nets,
                    iloc,
                );
            } else if let Some(&sub_mod) = self.mod_map.get(inst.module_name.as_str()) {
                let (sub_cid, sub_scope) = self.instantiate_module(
                    sub_mod,
                    Some(cid),
                    &inst.inst_name,
                    &inst.raw_name,
                    &child_dot,
                    &child_upf,
                    false,
                );
                self.stitch_child_module_conns(
                    cid,
                    dot_path,
                    upf_path,
                    sub_cid,
                    inst,
                    &mut local_nets,
                    iloc,
                );
                child_scopes.push(sub_scope);
            } else {
                // Unknown blackbox module
                self.instantiate_unknown_blackbox(
                    cid,
                    dot_path,
                    upf_path,
                    inst,
                    &child_dot,
                    &child_upf,
                    &mut local_nets,
                    iloc,
                );
            }
        }

        scope_ports.extend(scope_signals);
        let scope = Scope {
            path: dot_path.to_string(),
            name: inst_name.to_string(),
            module: m.name.clone(),
            signals: scope_ports,
            children: child_scopes,
        };
        (cid, scope)
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate_liberty_leaf(
        &mut self,
        parent: CellId,
        parent_dot: &str,
        parent_upf: &str,
        inst: &ParsedInst,
        lib_cell: &LibertyCell,
        child_dot: &str,
        child_upf: &str,
        parent_nets: &mut HashMap<String, NetSegId>,
        loc: svlang::Loc,
    ) {
        let cid = CellId(self.graph.cells.len() as u32);
        let is_special = lib_cell.is_isolation_cell
            || lib_cell.is_level_shifter
            || lib_cell.is_retention()
            || lib_cell.is_power_switch()
            || lib_cell.is_macro_cell
            || lib_cell.is_always_on
            || lib_cell.is_sequential();

        self.graph.cells.push(CellNode {
            id: cid,
            inst_id: None,
            parent: Some(parent),
            children: Vec::new(),
            name: inst.inst_name.clone(),
            raw_name: inst.raw_name.clone(),
            path: child_dot.to_string(),
            upf_path: child_upf.to_string(),
            module: inst.module_name.clone(),
            is_top: false,
            is_interface: false,
            is_checker: false,
            is_blackbox: true,
            is_bound: false,
            pins: Vec::with_capacity(inst.conns.len()),
            nets: Vec::new(),
            flops: Vec::new(),
            latches: Vec::new(),
            loc,
        });
        self.graph.cells[parent.0 as usize].children.push(cid);
        self.graph.cell_by_path.insert(child_dot.to_string(), cid);
        self.graph.cell_by_upf.insert(child_upf.to_string(), cid);

        // Wire every explicitly connected pin in `inst.conns`.
        let mut connected_names: Vec<&str> = Vec::with_capacity(inst.conns.len());
        let mut seq_q_net: Option<NetSegId> = None;

        for (arg_idx, conn) in inst.conns.iter().enumerate() {
            let pin_name = match conn.pin_name.as_deref() {
                Some(n) => n,
                None => lib_cell
                    .pins
                    .get(arg_idx)
                    .map(|p| p.name.as_str())
                    .unwrap_or("P"),
            };
            connected_names.push(pin_name);

            let (dir, width) = if let Some(sp) = lib_cell.pin(pin_name) {
                let d = match sp.direction {
                    PinDir::Input => Some(PortDir::Input),
                    PinDir::Output => Some(PortDir::Output),
                    PinDir::Inout => Some(PortDir::Inout),
                    PinDir::Internal => None,
                };
                (d, sp.width.max(1))
            } else if let Some(pg) = lib_cell.pg_pin(pin_name) {
                let d = if matches!(pg.pg_type, PgType::InternalPower | PgType::InternalGround) {
                    Some(PortDir::Output)
                } else {
                    Some(PortDir::Input)
                };
                (d, 1)
            } else {
                (None, 1)
            };

            let cloc = make_loc(loc.file, conn.line);
            let pid = PinId(self.graph.pins.len() as u32);
            let pin_upf = if is_special {
                join_upf(child_upf, pin_name)
            } else {
                String::new()
            };
            let is_empty = matches!(conn.expr, ParsedExpr::Empty);
            self.graph.pins.push(PinNode {
                id: pid,
                cell: cid,
                symbol: None,
                name: pin_name.to_string(),
                upf_path: pin_upf.clone(),
                width,
                packed_bounds: None,
                dir,
                internal_net: None,
                external_conns: Vec::new(),
                unconnected: is_empty,
                loc: cloc,
            });
            self.graph.cells[cid.0 as usize].pins.push(pid);
            if is_special && !pin_upf.is_empty() {
                self.graph.pin_by_upf.insert(pin_upf, pid);
            }

            if !is_empty {
                let bnd = self.build_binding(
                    parent,
                    parent_dot,
                    parent_upf,
                    width,
                    &conn.expr,
                    parent_nets,
                    cloc,
                );
                if lib_cell.is_sequential() && dir == Some(PortDir::Output) {
                    if let Some(pn) = bnd.parent_net {
                        seq_q_net = Some(pn);
                    }
                }
                self.attach_leaf_pin_binding(pid, dir, bnd);
            }
        }

        // Materialize omitted Liberty `pg_pin`s & `retention_pin`s as `unconnected: true`
        // so Stage 3/4 detect `PG_PIN_UNCONNECTED` / `RET_PIN_UNCONNECTED` identically to `svlang`.
        for pg in &lib_cell.pg_pins {
            if !connected_names.contains(&pg.name.as_str()) {
                let pid = PinId(self.graph.pins.len() as u32);
                let pin_upf = join_upf(child_upf, &pg.name);
                self.graph.pins.push(PinNode {
                    id: pid,
                    cell: cid,
                    symbol: None,
                    name: pg.name.clone(),
                    upf_path: pin_upf,
                    width: 1,
                    packed_bounds: None,
                    dir: Some(PortDir::Input),
                    internal_net: None,
                    external_conns: Vec::new(),
                    unconnected: true,
                    loc,
                });
                self.graph.cells[cid.0 as usize].pins.push(pid);
            }
        }
        if lib_cell.is_retention() {
            for sp in &lib_cell.pins {
                if sp.retention_pin.is_some() && !connected_names.contains(&sp.name.as_str()) {
                    let pid = PinId(self.graph.pins.len() as u32);
                    let pin_upf = join_upf(child_upf, &sp.name);
                    self.graph.pins.push(PinNode {
                        id: pid,
                        cell: cid,
                        symbol: None,
                        name: sp.name.clone(),
                        upf_path: pin_upf,
                        width: sp.width.max(1),
                        packed_bounds: None,
                        dir: Some(PortDir::Input),
                        internal_net: None,
                        external_conns: Vec::new(),
                        unconnected: true,
                        loc,
                    });
                    self.graph.cells[cid.0 as usize].pins.push(pid);
                }
            }
        }

        // Record sequential element metadata for UPF `set_retention_elements` / Stage 3.
        if lib_cell.is_sequential() {
            if let Some(q_nid) = seq_q_net {
                let w = self.graph.nets[q_nid.0 as usize].width;
                let seq = SeqElement {
                    net: q_nid,
                    range: BitRange::full(w),
                    full_path: child_dot.to_string(),
                    upf_path: child_upf.to_string(),
                    is_latch: lib_cell.has_latch && !lib_cell.has_ff,
                    clock: None,
                    async_reset: None,
                    async_reset_range: None,
                    async_reset_active_high: None,
                    loc,
                };
                if seq.is_latch {
                    self.graph.cells[cid.0 as usize].latches.push(seq);
                } else {
                    self.graph.cells[cid.0 as usize].flops.push(seq);
                }
            }
        }
    }

    pub(super) fn ensure_local_net(
        &mut self,
        cid: CellId,
        dot_path: &str,
        upf_path: &str,
        name: &str,
        local_nets: &mut HashMap<String, NetSegId>,
        loc: svlang::Loc,
    ) -> NetSegId {
        if let Some(&nid) = local_nets.get(name) {
            return nid;
        }
        let nid = self.alloc_net(
            cid,
            dot_path,
            upf_path,
            name,
            1,
            None,
            NetSegKind::Wire,
            loc,
        );
        local_nets.insert(name.to_string(), nid);
        nid
    }

    #[allow(clippy::too_many_arguments)]
    fn alloc_net(
        &mut self,
        cid: CellId,
        dot_path: &str,
        upf_path: &str,
        name: &str,
        width: u32,
        packed_bounds: Option<(i128, i128)>,
        kind: NetSegKind,
        loc: svlang::Loc,
    ) -> NetSegId {
        let nid = NetSegId(self.graph.nets.len() as u32);
        let full_path = format!("{dot_path}.{name}");
        let net_upf = join_upf(upf_path, name);
        self.graph.nets.push(NetSegment {
            id: nid,
            cell: cid,
            symbol: None,
            name: name.to_string(),
            full_path: full_path.clone(),
            upf_path: net_upf.clone(),
            width: width.max(1),
            packed_bounds,
            kind,
            is_seq_state: false,
            drivers: Vec::new(),
            loads: Vec::new(),
            loc,
        });
        self.graph.cells[cid.0 as usize].nets.push(nid);
        self.graph.net_by_path.insert(full_path, nid);
        self.graph.net_by_upf.insert(net_upf, nid);
        nid
    }
}

fn join_upf(prefix: &str, leaf: &str) -> String {
    if prefix.is_empty() {
        leaf.to_string()
    } else if leaf.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}/{leaf}")
    }
}
