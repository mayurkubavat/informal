//! Hierarchical pin/PG connection stitching and continuous-assignment lowering
//! for `gate_fast::elaborate`.

use std::collections::HashMap;

use svlang::semantic::PortDir;

use super::elaborate::{make_loc, ElabCtx};
use super::parser::{ParsedExpr, ParsedInst};
use crate::struct_graph::{
    BitRange, CellId, CellNode, DriverEdge, DriverOrigin, ExprOpSummary, LoadEdge, NetSegId,
    PinConnBinding, PinId, PinNode,
};

impl<'a> ElabCtx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn instantiate_unknown_blackbox(
        &mut self,
        parent: CellId,
        parent_dot: &str,
        parent_upf: &str,
        inst: &ParsedInst,
        child_dot: &str,
        child_upf: &str,
        parent_nets: &mut HashMap<String, NetSegId>,
        loc: svlang::Loc,
    ) {
        let cid = CellId(self.graph.cells.len() as u32);
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
            pins: Vec::new(),
            nets: Vec::new(),
            flops: Vec::new(),
            latches: Vec::new(),
            loc,
        });
        self.graph.cells[parent.0 as usize].children.push(cid);
        self.graph.cell_by_path.insert(child_dot.to_string(), cid);
        self.graph.cell_by_upf.insert(child_upf.to_string(), cid);

        for (idx, conn) in inst.conns.iter().enumerate() {
            let pname = conn.pin_name.clone().unwrap_or_else(|| format!("p_{idx}"));
            let pid = PinId(self.graph.pins.len() as u32);
            let is_empty = matches!(conn.expr, ParsedExpr::Empty);
            self.graph.pins.push(PinNode {
                id: pid,
                cell: cid,
                symbol: None,
                name: pname,
                upf_path: String::new(),
                width: 1,
                packed_bounds: None,
                dir: None,
                internal_net: None,
                external_conns: Vec::new(),
                unconnected: is_empty,
                loc,
            });
            self.graph.cells[cid.0 as usize].pins.push(pid);
            if !is_empty {
                let bnd = self.build_binding(
                    parent,
                    parent_dot,
                    parent_upf,
                    1,
                    &conn.expr,
                    parent_nets,
                    loc,
                );
                self.attach_leaf_pin_binding(pid, None, bnd);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn stitch_child_module_conns(
        &mut self,
        parent: CellId,
        parent_dot: &str,
        parent_upf: &str,
        child: CellId,
        inst: &ParsedInst,
        parent_nets: &mut HashMap<String, NetSegId>,
        loc: svlang::Loc,
    ) {
        let pin_ids = self.graph.cells[child.0 as usize].pins.clone();
        for (port_idx, pid) in pin_ids.into_iter().enumerate() {
            let pname = self.graph.pins[pid.0 as usize].name.clone();
            let pdir = self.graph.pins[pid.0 as usize].dir;
            let pwidth = self.graph.pins[pid.0 as usize].width;
            let inet = self.graph.pins[pid.0 as usize].internal_net;

            let conn_opt = inst
                .conns
                .iter()
                .find(|c| c.pin_name.as_deref() == Some(pname.as_str()))
                .or_else(|| inst.conns.get(port_idx).filter(|c| c.pin_name.is_none()));
            let Some(conn) = conn_opt else {
                self.graph.pins[pid.0 as usize].unconnected = true;
                continue;
            };
            if matches!(conn.expr, ParsedExpr::Empty) {
                self.graph.pins[pid.0 as usize].unconnected = true;
                continue;
            }
            let cloc = make_loc(loc.file, conn.line);
            let bnd = self.build_binding(
                parent,
                parent_dot,
                parent_upf,
                pwidth,
                &conn.expr,
                parent_nets,
                cloc,
            );
            self.graph.pins[pid.0 as usize]
                .external_conns
                .push(bnd.clone());
            let Some(inet_id) = inet else {
                continue;
            };
            match pdir {
                Some(PortDir::Input) => {
                    if let Some(pnet) = bnd.parent_net {
                        self.graph.nets[inet_id.0 as usize]
                            .drivers
                            .push(DriverEdge {
                                dst_range: bnd.pin_range,
                                src_net: Some(pnet),
                                src_range: Some(bnd.parent_range),
                                origin: DriverOrigin::ParentPort(pid),
                                op: bnd.expr_op.clone(),
                                extra_deps: bnd.expr_deps.clone(),
                                dep_ops: bnd.dep_ops.clone(),
                                inline_clamp: None,
                                loc: bnd.loc,
                            });
                        self.graph.nets[pnet.0 as usize].loads.push(LoadEdge {
                            src_range: bnd.parent_range,
                            dst_net: Some(inet_id),
                            dst_pin: Some(pid),
                            dst_range: bnd.pin_range,
                            op: bnd.expr_op,
                            loc: bnd.loc,
                        });
                    } else {
                        self.graph.nets[inet_id.0 as usize]
                            .drivers
                            .push(DriverEdge {
                                dst_range: bnd.pin_range,
                                src_net: None,
                                src_range: None,
                                origin: DriverOrigin::ParentPort(pid),
                                op: bnd.expr_op.clone(),
                                extra_deps: bnd.expr_deps.clone(),
                                dep_ops: bnd.dep_ops.clone(),
                                inline_clamp: None,
                                loc: bnd.loc,
                            });
                        for &(dn, dr) in &bnd.expr_deps {
                            self.graph.nets[dn.0 as usize].loads.push(LoadEdge {
                                src_range: dr,
                                dst_net: Some(inet_id),
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
                        self.graph.nets[pnet.0 as usize].drivers.push(DriverEdge {
                            dst_range: bnd.parent_range,
                            src_net: Some(inet_id),
                            src_range: Some(bnd.pin_range),
                            origin: DriverOrigin::ChildPin(pid),
                            op: ExprOpSummary::Identity,
                            extra_deps: Vec::new(),
                            dep_ops: Vec::new(),
                            inline_clamp: None,
                            loc: bnd.loc,
                        });
                        self.graph.nets[inet_id.0 as usize].loads.push(LoadEdge {
                            src_range: bnd.pin_range,
                            dst_net: Some(pnet),
                            dst_pin: None,
                            dst_range: bnd.parent_range,
                            op: ExprOpSummary::Identity,
                            loc: bnd.loc,
                        });
                    }
                }
                Some(PortDir::Inout) | None | Some(PortDir::Ref) => {
                    if let Some(pnet) = bnd.parent_net {
                        self.graph.nets[pnet.0 as usize].loads.push(LoadEdge {
                            src_range: bnd.parent_range,
                            dst_net: Some(inet_id),
                            dst_pin: Some(pid),
                            dst_range: bnd.pin_range,
                            op: ExprOpSummary::Identity,
                            loc: bnd.loc,
                        });
                        self.graph.nets[inet_id.0 as usize].loads.push(LoadEdge {
                            src_range: bnd.pin_range,
                            dst_net: Some(pnet),
                            dst_pin: Some(pid),
                            dst_range: bnd.parent_range,
                            op: ExprOpSummary::Identity,
                            loc: bnd.loc,
                        });
                    }
                }
            }
        }
    }

    pub(super) fn attach_leaf_pin_binding(
        &mut self,
        pid: PinId,
        dir: Option<PortDir>,
        bnd: PinConnBinding,
    ) {
        self.graph.pins[pid.0 as usize]
            .external_conns
            .push(bnd.clone());
        match dir {
            Some(PortDir::Output) => {
                if let Some(pnet) = bnd.parent_net {
                    self.graph.nets[pnet.0 as usize].drivers.push(DriverEdge {
                        dst_range: bnd.parent_range,
                        src_net: None,
                        src_range: None,
                        origin: DriverOrigin::ChildPin(pid),
                        op: ExprOpSummary::Identity,
                        extra_deps: Vec::new(),
                        dep_ops: Vec::new(),
                        inline_clamp: None,
                        loc: bnd.loc,
                    });
                }
            }
            _ => {
                if let Some(pnet) = bnd.parent_net {
                    self.graph.nets[pnet.0 as usize].loads.push(LoadEdge {
                        src_range: bnd.parent_range,
                        dst_net: None,
                        dst_pin: Some(pid),
                        dst_range: bnd.pin_range,
                        op: bnd.expr_op,
                        loc: bnd.loc,
                    });
                } else {
                    for &(dn, dr) in &bnd.expr_deps {
                        self.graph.nets[dn.0 as usize].loads.push(LoadEdge {
                            src_range: dr,
                            dst_net: None,
                            dst_pin: Some(pid),
                            dst_range: bnd.pin_range,
                            op: bnd.expr_op.clone(),
                            loc: bnd.loc,
                        });
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_binding(
        &mut self,
        parent: CellId,
        parent_dot: &str,
        parent_upf: &str,
        pin_width: u32,
        expr: &ParsedExpr,
        parent_nets: &mut HashMap<String, NetSegId>,
        loc: svlang::Loc,
    ) -> PinConnBinding {
        let pin_range = BitRange::full(pin_width);
        match expr {
            ParsedExpr::Empty => PinConnBinding {
                pin_range,
                parent_net: None,
                parent_range: pin_range,
                expr_op: ExprOpSummary::Comb,
                expr_deps: Vec::new(),
                dep_ops: Vec::new(),
                inline_clamp: None,
                loc,
            },
            ParsedExpr::Const {
                is_zero,
                is_all_ones,
            } => PinConnBinding {
                pin_range,
                parent_net: None,
                parent_range: pin_range,
                expr_op: ExprOpSummary::Const {
                    is_zero: *is_zero,
                    is_all_ones: *is_all_ones,
                },
                expr_deps: Vec::new(),
                dep_ops: Vec::new(),
                inline_clamp: None,
                loc,
            },
            ParsedExpr::Ref { net, range, invert } => {
                let nid =
                    self.ensure_local_net(parent, parent_dot, parent_upf, net, parent_nets, loc);
                let nw = self.graph.nets[nid.0 as usize].width;
                let pr = range
                    .map(|(lo, hi)| BitRange { lo, hi })
                    .unwrap_or_else(|| BitRange::full(nw));
                let op = if *invert {
                    ExprOpSummary::Invert
                } else {
                    ExprOpSummary::Identity
                };
                PinConnBinding {
                    pin_range,
                    parent_net: Some(nid),
                    parent_range: pr,
                    expr_op: op.clone(),
                    expr_deps: vec![(nid, pr)],
                    dep_ops: vec![op],
                    inline_clamp: None,
                    loc,
                }
            }
            ParsedExpr::Compound { refs } => {
                let mut deps = Vec::with_capacity(refs.len());
                for (rname, rrng) in refs {
                    let nid = self.ensure_local_net(
                        parent,
                        parent_dot,
                        parent_upf,
                        rname,
                        parent_nets,
                        loc,
                    );
                    let nw = self.graph.nets[nid.0 as usize].width;
                    let rr = rrng
                        .map(|(lo, hi)| BitRange { lo, hi })
                        .unwrap_or_else(|| BitRange::full(nw));
                    deps.push((nid, rr));
                }
                let dep_ops = vec![ExprOpSummary::Comb; deps.len()];
                PinConnBinding {
                    pin_range,
                    parent_net: None,
                    parent_range: pin_range,
                    expr_op: ExprOpSummary::Comb,
                    expr_deps: deps,
                    dep_ops,
                    inline_clamp: None,
                    loc,
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn wire_assign_expr(
        &mut self,
        cid: CellId,
        dot_path: &str,
        upf_path: &str,
        dst_nid: NetSegId,
        dst_range: BitRange,
        rhs: &ParsedExpr,
        local_nets: &mut HashMap<String, NetSegId>,
        loc: svlang::Loc,
    ) {
        match rhs {
            ParsedExpr::Empty => {}
            ParsedExpr::Const {
                is_zero,
                is_all_ones,
            } => {
                self.graph.nets[dst_nid.0 as usize]
                    .drivers
                    .push(DriverEdge {
                        dst_range,
                        src_net: None,
                        src_range: None,
                        origin: DriverOrigin::Assign,
                        op: ExprOpSummary::Const {
                            is_zero: *is_zero,
                            is_all_ones: *is_all_ones,
                        },
                        extra_deps: Vec::new(),
                        dep_ops: Vec::new(),
                        inline_clamp: None,
                        loc,
                    });
            }
            ParsedExpr::Ref { net, range, invert } => {
                let src_nid = self.ensure_local_net(cid, dot_path, upf_path, net, local_nets, loc);
                let sw = self.graph.nets[src_nid.0 as usize].width;
                let sr = range
                    .map(|(lo, hi)| BitRange { lo, hi })
                    .unwrap_or_else(|| BitRange::full(sw));
                let op = if *invert {
                    ExprOpSummary::Invert
                } else {
                    ExprOpSummary::Identity
                };
                self.graph.nets[dst_nid.0 as usize]
                    .drivers
                    .push(DriverEdge {
                        dst_range,
                        src_net: Some(src_nid),
                        src_range: Some(sr),
                        origin: DriverOrigin::Assign,
                        op: op.clone(),
                        extra_deps: Vec::new(),
                        dep_ops: Vec::new(),
                        inline_clamp: None,
                        loc,
                    });
                self.graph.nets[src_nid.0 as usize].loads.push(LoadEdge {
                    src_range: sr,
                    dst_net: Some(dst_nid),
                    dst_pin: None,
                    dst_range,
                    op,
                    loc,
                });
            }
            ParsedExpr::Compound { refs } => {
                let mut deps = Vec::with_capacity(refs.len());
                for (rname, rrng) in refs {
                    let nid =
                        self.ensure_local_net(cid, dot_path, upf_path, rname, local_nets, loc);
                    let nw = self.graph.nets[nid.0 as usize].width;
                    let rr = rrng
                        .map(|(lo, hi)| BitRange { lo, hi })
                        .unwrap_or_else(|| BitRange::full(nw));
                    deps.push((nid, rr));
                }
                let dep_ops = vec![ExprOpSummary::Comb; deps.len()];
                self.graph.nets[dst_nid.0 as usize]
                    .drivers
                    .push(DriverEdge {
                        dst_range,
                        src_net: None,
                        src_range: None,
                        origin: DriverOrigin::Assign,
                        op: ExprOpSummary::Comb,
                        extra_deps: deps.clone(),
                        dep_ops,
                        inline_clamp: None,
                        loc,
                    });
                for &(dn, dr) in &deps {
                    self.graph.nets[dn.0 as usize].loads.push(LoadEdge {
                        src_range: dr,
                        dst_net: Some(dst_nid),
                        dst_pin: None,
                        dst_range,
                        op: ExprOpSummary::Comb,
                        loc,
                    });
                }
            }
        }
    }
}
