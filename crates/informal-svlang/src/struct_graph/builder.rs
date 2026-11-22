//! Three-pass builder constructing [`StructuralGraph`] from `svlang::ElaboratedDesign`.

use std::collections::{BTreeSet, HashMap};

use svlang::semantic::expr::{ExprArena, ExprBinder};
use svlang::semantic::{
    ElabItemKind, ElaboratedDesign, InstanceId, PortDir, RtlType, SymbolId, SymbolKind,
};
use svlang::syntax::Tree;
use svlang::Loc;

use super::{
    normalize_query_path, normalize_sv_leaf, BitRange, CellId, CellNode, DriverEdge, DriverOrigin,
    ExprOpSummary, InlineClampInfo, LoadEdge, NetSegId, NetSegKind, NetSegment, PinConnBinding,
    PinId, PinNode, StructuralGraph,
};

pub(crate) struct Builder<'a> {
    pub(crate) trees: &'a [Tree],
    pub(crate) design: &'a ElaboratedDesign,
    pub(crate) graph: StructuralGraph,
    pub(crate) unpacked_field_nets: HashMap<(SymbolId, String), NetSegId>,
    pub(crate) unpacked_elem_nets: HashMap<(SymbolId, i128), NetSegId>,
}

pub(crate) fn build_structural_graph(
    trees: &[Tree],
    design: &ElaboratedDesign,
    top: Option<&str>,
) -> StructuralGraph {
    let mut selected_tops = Vec::new();
    if let Some(t) = top {
        for &tid in &design.tops {
            let inst = design.instance(tid);
            if inst.definition == t || inst.name == t {
                selected_tops.push(tid);
            }
        }
    }
    if selected_tops.is_empty() {
        selected_tops = design.tops.clone();
    }
    let top_names: Vec<String> = selected_tops
        .iter()
        .map(|&id| normalize_sv_leaf(&design.instance(id).name))
        .collect();

    let mut b = Builder {
        trees,
        design,
        graph: StructuralGraph {
            files: design.files.clone(),
            top_names,
            tops: Vec::new(),
            cells: Vec::new(),
            pins: Vec::new(),
            nets: Vec::new(),
            cell_by_path: HashMap::new(),
            cell_by_upf: HashMap::new(),
            net_by_path: HashMap::new(),
            net_by_upf: HashMap::new(),
            sym_to_net: HashMap::new(),
            pin_by_upf: HashMap::new(),
            inst_to_cell: HashMap::new(),
        },
        unpacked_field_nets: HashMap::new(),
        unpacked_elem_nets: HashMap::new(),
    };

    for tid in selected_tops {
        let cid = b.alloc_cell_recursive(tid, None);
        b.graph.tops.push(cid);
    }

    b.stitch_port_conns();
    b.collect_instance_items();
    b.graph
}

impl<'a> Builder<'a> {
    fn alloc_cell_recursive(&mut self, inst_id: InstanceId, parent: Option<CellId>) -> CellId {
        let inst = self.design.instance(inst_id);
        let cid = CellId(self.graph.cells.len() as u32);
        let is_top = parent.is_none();
        let norm_name = normalize_sv_leaf(&inst.name);
        let upf_path = match parent {
            None => String::new(),
            Some(pid) => join_upf(&self.graph.cells[pid.0 as usize].upf_path, &inst.name),
        };

        self.graph.cells.push(CellNode {
            id: cid,
            inst_id: Some(inst_id),
            parent,
            children: Vec::new(),
            name: norm_name,
            raw_name: inst.name.clone(),
            path: inst.path.clone(),
            upf_path: upf_path.clone(),
            module: inst.definition.clone(),
            is_top,
            is_interface: inst.is_interface,
            is_checker: inst.is_checker,
            is_blackbox: inst.is_blackbox_stub,
            is_bound: inst.is_bound,
            pins: Vec::new(),
            nets: Vec::new(),
            flops: Vec::new(),
            latches: Vec::new(),
            loc: inst.loc,
        });
        self.graph.inst_to_cell.insert(inst_id, cid);
        self.graph.cell_by_path.insert(inst.path.clone(), cid);
        self.graph
            .cell_by_upf
            .insert(normalize_query_path(&inst.path), cid);
        self.graph.cell_by_upf.insert(upf_path.clone(), cid);

        if let Some(pid) = parent {
            self.graph.cells[pid.0 as usize].children.push(cid);
        }

        for &sid in &inst.symbols {
            let sym = self.design.symbol(sid);
            if matches!(sym.kind, SymbolKind::Param { .. } | SymbolKind::Genvar)
                || matches!(
                    sym.ty,
                    RtlType::Event
                        | RtlType::Void
                        | RtlType::NonSynthesizable(_)
                        | RtlType::Interface { .. }
                )
            {
                continue;
            }
            let base_kind = match &sym.kind {
                SymbolKind::Port { dir, .. } => NetSegKind::Port(*dir),
                SymbolKind::Var => NetSegKind::Var,
                SymbolKind::Net { net_type } => match net_type.as_str() {
                    "supply0" => NetSegKind::Supply0,
                    "supply1" => NetSegKind::Supply1,
                    _ => NetSegKind::Wire,
                },
                SymbolKind::Param { .. } | SymbolKind::Genvar => continue,
            };
            self.alloc_symbol_nets(
                cid,
                sid,
                &sym.ty,
                &sym.rel_path,
                &sym.full_path,
                base_kind,
                sym.packed_bounds,
                sym.loc,
                true,
                is_top,
            );
        }

        if inst.is_blackbox_stub && inst.ports.is_empty() {
            let mut seen_names = BTreeSet::new();
            for conn in &inst.port_conns {
                if conn.wildcard {
                    continue;
                }
                let pname = conn
                    .port_name
                    .as_deref()
                    .map(normalize_sv_leaf)
                    .unwrap_or_else(|| format!("port_{}", conn.arg_index));
                if !seen_names.insert(pname.clone()) {
                    continue;
                }
                let full_path = format!("{}.{pname}", inst.path);
                let upf_p = join_upf(&upf_path, &pname);
                let nid = self.push_net(
                    cid,
                    None,
                    pname.clone(),
                    full_path,
                    upf_p.clone(),
                    1,
                    None,
                    NetSegKind::BlackboxStub,
                    conn.loc,
                );
                self.push_pin(cid, None, pname, upf_p, 1, None, None, Some(nid), conn.loc);
            }
        }

        for ch_id in inst.children.clone() {
            self.alloc_cell_recursive(ch_id, Some(cid));
        }
        cid
    }

    #[allow(clippy::too_many_arguments)]
    fn alloc_symbol_nets(
        &mut self,
        cid: CellId,
        sid: SymbolId,
        ty: &RtlType,
        rel_name: &str,
        full_path: &str,
        kind: NetSegKind,
        packed_bounds: Option<(i128, i128)>,
        loc: Loc,
        is_root_sym: bool,
        is_top: bool,
    ) {
        if let Some(w) = ty.bit_width() {
            let width = w.max(1);
            let cell_upf = &self.graph.cells[cid.0 as usize].upf_path;
            let leaf_upf = if rel_name.starts_with('\\') {
                normalize_sv_leaf(rel_name)
            } else {
                normalize_query_path(rel_name)
            };
            let upf_p = if cell_upf.is_empty() {
                leaf_upf
            } else if leaf_upf.is_empty() {
                cell_upf.clone()
            } else {
                format!("{cell_upf}/{leaf_upf}")
            };
            let nid = self.push_net(
                cid,
                is_root_sym.then_some(sid),
                rel_name.to_string(),
                full_path.to_string(),
                upf_p.clone(),
                width,
                packed_bounds,
                kind,
                loc,
            );
            if is_root_sym {
                self.graph.sym_to_net.insert(sid, nid);
                if let NetSegKind::Port(dir) = kind {
                    let pid = self.push_pin(
                        cid,
                        Some(sid),
                        rel_name.to_string(),
                        upf_p,
                        width,
                        packed_bounds,
                        Some(dir),
                        Some(nid),
                        loc,
                    );
                    if is_top && matches!(dir, PortDir::Input | PortDir::Inout) {
                        self.graph.nets[nid.0 as usize].drivers.push(DriverEdge {
                            dst_range: BitRange::full(width),
                            src_net: None,
                            src_range: None,
                            origin: DriverOrigin::PrimaryInput(pid),
                            op: ExprOpSummary::Identity,
                            extra_deps: Vec::new(),
                            dep_ops: Vec::new(),
                            inline_clamp: None,
                            loc,
                        });
                    }
                }
            }
            return;
        }
        match ty {
            RtlType::UnpackedStruct { fields, .. } => {
                for (fname, fty) in fields {
                    let prev_len = self.graph.nets.len();
                    self.alloc_symbol_nets(
                        cid,
                        sid,
                        fty,
                        &format!("{rel_name}.{fname}"),
                        &format!("{full_path}.{fname}"),
                        kind,
                        None,
                        loc,
                        false,
                        false,
                    );
                    if self.graph.nets.len() > prev_len {
                        self.unpacked_field_nets
                            .insert((sid, fname.clone()), NetSegId(prev_len as u32));
                    }
                }
            }
            RtlType::UnpackedArray { elem, dims } if dims.len() == 1 && elem.is_packed() => {
                let (l, r) = dims[0];
                let cnt = (l - r).unsigned_abs().saturating_add(1);
                if cnt > 64 {
                    let w = elem.bit_width().unwrap_or(1).max(1);
                    let cell_upf = &self.graph.cells[cid.0 as usize].upf_path;
                    let leaf_upf = if rel_name.starts_with('\\') {
                        normalize_sv_leaf(rel_name)
                    } else {
                        normalize_query_path(rel_name)
                    };
                    let upf_p = if cell_upf.is_empty() {
                        leaf_upf
                    } else if leaf_upf.is_empty() {
                        cell_upf.clone()
                    } else {
                        format!("{cell_upf}/{leaf_upf}")
                    };
                    let nid = self.push_net(
                        cid,
                        Some(sid),
                        rel_name.to_string(),
                        full_path.to_string(),
                        upf_p,
                        w,
                        None,
                        NetSegKind::Memory,
                        loc,
                    );
                    self.graph.sym_to_net.insert(sid, nid);
                    return;
                }
                let lo = l.min(r);
                let hi = l.max(r).min(lo + 4095);
                for idx in lo..=hi {
                    let prev_len = self.graph.nets.len();
                    self.alloc_symbol_nets(
                        cid,
                        sid,
                        elem,
                        &format!("{rel_name}[{idx}]"),
                        &format!("{full_path}[{idx}]"),
                        kind,
                        None,
                        loc,
                        false,
                        false,
                    );
                    if self.graph.nets.len() > prev_len {
                        self.unpacked_elem_nets
                            .insert((sid, idx), NetSegId(prev_len as u32));
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_net(
        &mut self,
        cell: CellId,
        symbol: Option<SymbolId>,
        name: String,
        full_path: String,
        upf_path: String,
        width: u32,
        packed_bounds: Option<(i128, i128)>,
        kind: NetSegKind,
        loc: Loc,
    ) -> NetSegId {
        let nid = NetSegId(self.graph.nets.len() as u32);
        self.graph.nets.push(NetSegment {
            id: nid,
            cell,
            symbol,
            name,
            full_path: full_path.clone(),
            upf_path: upf_path.clone(),
            width,
            packed_bounds,
            kind,
            is_seq_state: false,
            drivers: Vec::new(),
            loads: Vec::new(),
            loc,
        });
        self.graph.cells[cell.0 as usize].nets.push(nid);
        self.graph.net_by_path.insert(full_path.clone(), nid);
        self.graph.net_by_upf.insert(upf_path, nid);
        self.graph
            .net_by_upf
            .entry(normalize_query_path(&full_path))
            .or_insert(nid);
        nid
    }

    #[allow(clippy::too_many_arguments)]
    fn push_pin(
        &mut self,
        cell: CellId,
        symbol: Option<SymbolId>,
        name: String,
        upf_path: String,
        width: u32,
        packed_bounds: Option<(i128, i128)>,
        dir: Option<PortDir>,
        internal_net: Option<NetSegId>,
        loc: Loc,
    ) -> PinId {
        let pid = PinId(self.graph.pins.len() as u32);
        self.graph.pins.push(PinNode {
            id: pid,
            cell,
            symbol,
            name,
            upf_path: upf_path.clone(),
            width,
            packed_bounds,
            dir,
            internal_net,
            external_conns: Vec::new(),
            unconnected: false,
            loc,
        });
        self.graph.cells[cell.0 as usize].pins.push(pid);
        self.graph.pin_by_upf.insert(upf_path, pid);
        pid
    }

    fn stitch_port_conns(&mut self) {
        let inst_ids: Vec<InstanceId> = self.graph.inst_to_cell.keys().copied().collect();
        for child_inst_id in inst_ids {
            let child_inst = self.design.instance(child_inst_id);
            let Some(parent_inst_id) = child_inst.parent else {
                continue;
            };
            let child_cell_id = self.graph.inst_to_cell[&child_inst_id];
            let pin_ids = self.graph.cells[child_cell_id.0 as usize].pins.clone();

            for (port_idx, pid) in pin_ids.into_iter().enumerate() {
                let pin = self.graph.pins[pid.0 as usize].clone();
                let conn_opt = child_inst
                    .port_conns
                    .iter()
                    .find(|c| c.port_name.as_deref() == Some(pin.name.as_str()))
                    .or_else(|| {
                        child_inst.port_conns.iter().find(|c| {
                            c.port_name.is_none() && !c.wildcard && c.arg_index == port_idx
                        })
                    })
                    .or_else(|| child_inst.port_conns.iter().find(|c| c.wildcard))
                    .cloned();

                let Some(conn) = conn_opt else {
                    self.graph.pins[pid.0 as usize].unconnected = true;
                    continue;
                };

                if conn.implicit_named || conn.wildcard {
                    let parent_inst = self.design.instance(parent_inst_id);
                    if let Some(psid) = parent_inst.resolve_lexical(&conn.scope_prefix, &pin.name) {
                        if let Some(&pnet) = self.graph.sym_to_net.get(&psid) {
                            let pw = self.graph.nets[pnet.0 as usize].width;
                            let eff_w = if pin.dir.is_none() {
                                let w = pw.max(1);
                                self.graph.pins[pid.0 as usize].width = w;
                                if let Some(inet) = pin.internal_net {
                                    self.graph.nets[inet.0 as usize].width = w;
                                }
                                w
                            } else {
                                pin.width.min(pw).max(1)
                            };
                            let pr = BitRange::full(eff_w);
                            let binding = PinConnBinding {
                                pin_range: pr,
                                parent_net: Some(pnet),
                                parent_range: pr,
                                expr_op: ExprOpSummary::Identity,
                                expr_deps: vec![(pnet, pr)],
                                dep_ops: vec![ExprOpSummary::Identity],
                                inline_clamp: None,
                                loc: conn.loc,
                            };
                            self.apply_pin_binding(pid, pin.dir, pin.internal_net, binding);
                        }
                    } else {
                        self.graph.pins[pid.0 as usize].unconnected = true;
                    }
                    continue;
                }

                let Some((tr, nd)) = conn.expr else {
                    self.graph.pins[pid.0 as usize].unconnected = true;
                    continue;
                };
                let mut arena = ExprArena::new();
                let eid_opt = {
                    let mut binder = ExprBinder::new(
                        self.trees,
                        self.design,
                        parent_inst_id,
                        &conn.scope_prefix,
                        &conn.gen_consts,
                        &mut arena,
                    );
                    if pin.dir == Some(PortDir::Input) {
                        if let Some(psym) = pin.symbol {
                            let port_ty = self.design.symbol(psym).ty.clone();
                            binder.bind_assigned(tr, nd, &port_ty)
                        } else {
                            binder.bind_self(tr, nd)
                        }
                    } else {
                        binder.bind_self(tr, nd)
                    }
                };
                if let Some(eid) = eid_opt {
                    let eff_w = if pin.dir.is_none() {
                        let w = arena.get(eid).ty.bit_width().unwrap_or(pin.width).max(1);
                        self.graph.pins[pid.0 as usize].width = w;
                        if let Some(inet) = pin.internal_net {
                            self.graph.nets[inet.0 as usize].width = w;
                        }
                        w
                    } else {
                        pin.width
                    };
                    let bindings =
                        self.decompose_pin_bindings(&arena, eid, BitRange::full(eff_w), conn.loc);
                    for bnd in bindings {
                        self.apply_pin_binding(pid, pin.dir, pin.internal_net, bnd);
                    }
                }
            }
        }
    }

    fn apply_pin_binding(
        &mut self,
        pid: PinId,
        dir: Option<PortDir>,
        internal_net: Option<NetSegId>,
        bnd: PinConnBinding,
    ) {
        self.graph.pins[pid.0 as usize]
            .external_conns
            .push(bnd.clone());
        let Some(inet) = internal_net else {
            return;
        };
        match dir {
            Some(PortDir::Input) => {
                if let Some(pnet) = bnd.parent_net {
                    self.add_driver_and_load(
                        inet,
                        bnd.pin_range,
                        Some(pnet),
                        Some(bnd.parent_range),
                        DriverOrigin::ParentPort(pid),
                        bnd.expr_op,
                        bnd.expr_deps,
                        bnd.dep_ops,
                        bnd.inline_clamp,
                        Some(pid),
                        bnd.loc,
                    );
                } else {
                    self.graph.nets[inet.0 as usize].drivers.push(DriverEdge {
                        dst_range: bnd.pin_range,
                        src_net: None,
                        src_range: None,
                        origin: DriverOrigin::ParentPort(pid),
                        op: bnd.expr_op.clone(),
                        extra_deps: bnd.expr_deps.clone(),
                        dep_ops: bnd.dep_ops.clone(),
                        inline_clamp: bnd.inline_clamp,
                        loc: bnd.loc,
                    });
                    for &(dep_n, dep_r) in &bnd.expr_deps {
                        self.graph.nets[dep_n.0 as usize].loads.push(LoadEdge {
                            src_range: dep_r,
                            dst_net: Some(inet),
                            dst_pin: Some(pid),
                            dst_range: bnd.pin_range,
                            op: bnd.expr_op.clone(),
                            loc: bnd.loc,
                        });
                    }
                }
            }
            Some(PortDir::Output) => {
                if let Some(pnet) = bnd.parent_net {
                    self.add_driver_and_load(
                        pnet,
                        bnd.parent_range,
                        Some(inet),
                        Some(bnd.pin_range),
                        DriverOrigin::ChildPin(pid),
                        ExprOpSummary::Identity,
                        Vec::new(),
                        Vec::new(),
                        None,
                        None,
                        bnd.loc,
                    );
                }
            }
            Some(PortDir::Inout) | None => {
                if let Some(pnet) = bnd.parent_net {
                    self.graph.nets[pnet.0 as usize].loads.push(LoadEdge {
                        src_range: bnd.parent_range,
                        dst_net: Some(inet),
                        dst_pin: Some(pid),
                        dst_range: bnd.pin_range,
                        op: ExprOpSummary::Identity,
                        loc: bnd.loc,
                    });
                    self.graph.nets[inet.0 as usize].loads.push(LoadEdge {
                        src_range: bnd.pin_range,
                        dst_net: Some(pnet),
                        dst_pin: Some(pid),
                        dst_range: bnd.parent_range,
                        op: ExprOpSummary::Identity,
                        loc: bnd.loc,
                    });
                } else {
                    for &(dep_n, dep_r) in &bnd.expr_deps {
                        self.graph.nets[dep_n.0 as usize].loads.push(LoadEdge {
                            src_range: dep_r,
                            dst_net: Some(inet),
                            dst_pin: Some(pid),
                            dst_range: bnd.pin_range,
                            op: bnd.expr_op.clone(),
                            loc: bnd.loc,
                        });
                    }
                }
            }
            Some(PortDir::Ref) => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_driver_and_load(
        &mut self,
        dst_net: NetSegId,
        dst_range: BitRange,
        src_net: Option<NetSegId>,
        src_range: Option<BitRange>,
        origin: DriverOrigin,
        op: ExprOpSummary,
        extra_deps: Vec<(NetSegId, BitRange)>,
        dep_ops: Vec<ExprOpSummary>,
        inline_clamp: Option<InlineClampInfo>,
        dst_pin: Option<PinId>,
        loc: Loc,
    ) {
        self.add_driver_and_load_with_load_op(
            dst_net,
            dst_range,
            src_net,
            src_range,
            origin,
            op,
            None,
            extra_deps,
            dep_ops,
            inline_clamp,
            dst_pin,
            loc,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_driver_and_load_with_load_op(
        &mut self,
        dst_net: NetSegId,
        dst_range: BitRange,
        src_net: Option<NetSegId>,
        src_range: Option<BitRange>,
        origin: DriverOrigin,
        op: ExprOpSummary,
        load_op: Option<ExprOpSummary>,
        extra_deps: Vec<(NetSegId, BitRange)>,
        dep_ops: Vec<ExprOpSummary>,
        inline_clamp: Option<InlineClampInfo>,
        dst_pin: Option<PinId>,
        loc: Loc,
    ) {
        self.graph.nets[dst_net.0 as usize]
            .drivers
            .push(DriverEdge {
                dst_range,
                src_net,
                src_range,
                origin,
                op: op.clone(),
                extra_deps: extra_deps.clone(),
                dep_ops,
                inline_clamp,
                loc,
            });
        let effective_load_op = load_op.unwrap_or(op);
        if let Some(sn) = src_net {
            let sr =
                src_range.unwrap_or_else(|| BitRange::full(self.graph.nets[sn.0 as usize].width));
            self.graph.nets[sn.0 as usize].loads.push(LoadEdge {
                src_range: sr,
                dst_net: Some(dst_net),
                dst_pin,
                dst_range,
                op: effective_load_op.clone(),
                loc,
            });
        }
        for (dn, dr) in extra_deps {
            if Some(dn) == src_net {
                continue;
            }
            self.graph.nets[dn.0 as usize].loads.push(LoadEdge {
                src_range: dr,
                dst_net: Some(dst_net),
                dst_pin,
                dst_range,
                op: effective_load_op.clone(),
                loc,
            });
        }
    }

    fn collect_instance_items(&mut self) {
        let inst_ids: Vec<InstanceId> = self.graph.inst_to_cell.keys().copied().collect();
        for inst_id in inst_ids {
            let inst = self.design.instance(inst_id);
            let cid = self.graph.inst_to_cell[&inst_id];
            for &sid in &inst.symbols {
                let sym = self.design.symbol(sid);
                let Some(&nid) = self.graph.sym_to_net.get(&sid) else {
                    continue;
                };
                if matches!(
                    self.graph.nets[nid.0 as usize].kind,
                    NetSegKind::Supply0 | NetSegKind::Supply1
                ) {
                    let is_zero = self.graph.nets[nid.0 as usize].kind == NetSegKind::Supply0;
                    let w = self.graph.nets[nid.0 as usize].width;
                    self.graph.nets[nid.0 as usize].drivers.push(DriverEdge {
                        dst_range: BitRange::full(w),
                        src_net: None,
                        src_range: None,
                        origin: DriverOrigin::Supply,
                        op: ExprOpSummary::Const {
                            is_zero,
                            is_all_ones: !is_zero,
                        },
                        extra_deps: Vec::new(),
                        dep_ops: Vec::new(),
                        inline_clamp: None,
                        loc: sym.loc,
                    });
                }
                if let Some((tr, init_nd)) = sym.init_node {
                    if !matches!(
                        sym.kind,
                        SymbolKind::Param { .. } | SymbolKind::Genvar | SymbolKind::Port { .. }
                    ) {
                        let mut arena = ExprArena::new();
                        let eid_opt = {
                            let mut binder = ExprBinder::new(
                                self.trees,
                                self.design,
                                inst_id,
                                &sym.init_scope_prefix,
                                &[],
                                &mut arena,
                            );
                            binder.bind_assigned(tr, init_nd, &sym.ty)
                        };
                        if let Some(eid) = eid_opt {
                            let w = self.graph.nets[nid.0 as usize].width;
                            self.emit_rhs_drivers(
                                &arena,
                                nid,
                                BitRange::full(w),
                                eid,
                                DriverOrigin::Assign,
                                sym.loc,
                            );
                        }
                    }
                }
            }
            for item in &inst.items {
                match item.kind {
                    ElabItemKind::ContinuousAssign => self.walk_continuous_assign(inst_id, item),
                    ElabItemKind::GateInst => self.walk_gate_inst(inst_id, item),
                    ElabItemKind::UdpInst => self.walk_udp_inst(inst_id, item),
                    ElabItemKind::NetAlias => self.walk_net_alias(inst_id, item),
                    ElabItemKind::ProceduralBlock => self.walk_procedural_block(inst_id, cid, item),
                    _ => {}
                }
            }
        }
    }
}

fn join_upf(base: &str, leaf: &str) -> String {
    let nleaf = normalize_sv_leaf(leaf);
    if base.is_empty() {
        nleaf
    } else {
        format!("{base}/{nleaf}")
    }
}
