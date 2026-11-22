//! Instance item walkers: continuous assigns, gates, UDPs, aliases, and procedural blocks.

use std::collections::BTreeSet;

use svlang::semantic::expr::{ExprArena, ExprBinder, ExprId, ExprKind};
use svlang::semantic::{CVal, ElabItem, InstanceId};
use svlang::syntax::{Child, NodeId, SyntaxKind, Tree};
use svlang::Loc;

use super::builder::Builder;
use super::{
    BitRange, CellId, DriverEdge, DriverOrigin, ExprOpSummary, NetSegId, NetSegKind, SeqElement,
};

impl<'a> Builder<'a> {
    pub(crate) fn walk_continuous_assign(&mut self, inst_id: InstanceId, item: &ElabItem) {
        let t = &self.trees[item.tree];
        for assign_expr in t
            .children(item.node)
            .filter(|&c| t.kind(c) == SyntaxKind::BinaryExpr)
        {
            let kids: Vec<NodeId> = t
                .children(assign_expr)
                .filter(|&c| t.kind(c) != SyntaxKind::Attribute)
                .collect();
            if kids.len() != 2 {
                continue;
            }
            let mut arena = ExprArena::new();
            let (lhs_eid, rhs_eid) = {
                let mut binder = ExprBinder::new(
                    self.trees,
                    self.design,
                    inst_id,
                    &item.scope_prefix,
                    &item.gen_consts,
                    &mut arena,
                );
                let Some(le) = binder.bind_self(item.tree, kids[0]) else {
                    continue;
                };
                let lhs_ty = binder.arena.get(le).ty.clone();
                let Some(re) = binder.bind_assigned(item.tree, kids[1], &lhs_ty) else {
                    continue;
                };
                (le, re)
            };
            self.drive_lhs_expr(&arena, lhs_eid, rhs_eid, DriverOrigin::Assign, item.loc);
        }
    }

    fn drive_lhs_expr(
        &mut self,
        arena: &ExprArena,
        lhs_eid: ExprId,
        rhs_eid: ExprId,
        origin: DriverOrigin,
        loc: Loc,
    ) {
        if let ExprKind::Concat(parts) = &arena.get(lhs_eid).kind {
            for &p in parts {
                self.drive_lhs_expr(arena, p, rhs_eid, origin.clone(), loc);
            }
            return;
        }
        if let Some((dst_net, dst_range)) = self.resolve_expr_slice(arena, lhs_eid) {
            self.emit_rhs_drivers(arena, dst_net, dst_range, rhs_eid, origin, loc);
        }
    }

    pub(crate) fn walk_gate_inst(&mut self, inst_id: InstanceId, item: &ElabItem) {
        let t = &self.trees[item.tree];
        let gate_kw = own_first_tok(t, item.node).unwrap_or("").to_string();
        let instances: Vec<NodeId> = t
            .children(item.node)
            .filter(|&c| t.kind(c) == SyntaxKind::Instance)
            .collect();
        for inst_node in instances {
            let Some(al) = t.child(inst_node, SyntaxKind::ArgList) else {
                continue;
            };
            let args: Vec<NodeId> = t
                .children(al)
                .filter(|&c| t.kind(c) != SyntaxKind::Attribute)
                .filter_map(|c| {
                    if t.kind(c) == SyntaxKind::Arg {
                        t.children(c).find(|&k| t.kind(k) != SyntaxKind::Attribute)
                    } else {
                        Some(c)
                    }
                })
                .collect();
            if args.len() < 2 {
                continue;
            }
            let mut arena = ExprArena::new();
            let bound = {
                let mut binder = ExprBinder::new(
                    self.trees,
                    self.design,
                    inst_id,
                    &item.scope_prefix,
                    &item.gen_consts,
                    &mut arena,
                );
                let mut out = Vec::with_capacity(args.len());
                for &a in &args {
                    if let Some(e) = binder.bind_self(item.tree, a) {
                        out.push(e);
                    }
                }
                out
            };
            if bound.len() != args.len() {
                continue;
            }
            let (out_eids, in_eids): (&[ExprId], &[ExprId]) = match gate_kw.as_str() {
                "buf" | "not" => (&bound[..bound.len() - 1], &bound[bound.len() - 1..]),
                _ => (&bound[..1], &bound[1..]),
            };
            let op = match gate_kw.as_str() {
                "buf" | "bufif0" | "bufif1" => ExprOpSummary::Identity,
                "not" | "notif0" | "notif1" => ExprOpSummary::Invert,
                "and" | "or" => ExprOpSummary::UnateGate {
                    is_inverting: false,
                },
                "nand" | "nor" => ExprOpSummary::UnateGate { is_inverting: true },
                _ => ExprOpSummary::Comb,
            };
            let mut deps = Vec::new();
            for &ie in in_eids {
                self.collect_expr_deps(&arena, ie, &mut deps);
            }
            let primary = (deps.len() == 1).then(|| deps[0]);
            for &oe in out_eids {
                if let Some((dst_net, dst_range)) = self.resolve_expr_slice(&arena, oe) {
                    self.add_driver_and_load(
                        dst_net,
                        dst_range,
                        primary.map(|p| p.0),
                        primary.map(|p| p.1),
                        DriverOrigin::Gate(gate_kw.clone()),
                        op.clone(),
                        deps.clone(),
                        Vec::new(),
                        None,
                        None,
                        item.loc,
                    );
                }
            }
        }
    }

    pub(crate) fn walk_udp_inst(&mut self, inst_id: InstanceId, item: &ElabItem) {
        let t = &self.trees[item.tree];
        let udp_name = t
            .child(item.node, SyntaxKind::DataType)
            .map(|dt| t.bare_text(dt).trim().to_string())
            .unwrap_or_else(|| "udp".to_string());
        for inst_node in t
            .children(item.node)
            .filter(|&c| t.kind(c) == SyntaxKind::Instance)
        {
            let Some(al) = t.child(inst_node, SyntaxKind::ArgList) else {
                continue;
            };
            let args: Vec<NodeId> = t
                .children(al)
                .filter_map(|c| {
                    (t.kind(c) == SyntaxKind::Arg)
                        .then(|| t.children(c).find(|&k| t.kind(k) != SyntaxKind::Attribute))
                        .flatten()
                })
                .collect();
            if args.len() < 2 {
                continue;
            }
            let mut arena = ExprArena::new();
            let (out_eid, in_eids) = {
                let mut binder = ExprBinder::new(
                    self.trees,
                    self.design,
                    inst_id,
                    &item.scope_prefix,
                    &item.gen_consts,
                    &mut arena,
                );
                let Some(oe) = binder.bind_self(item.tree, args[0]) else {
                    continue;
                };
                let mut ins = Vec::new();
                for &an in &args[1..] {
                    if let Some(ie) = binder.bind_self(item.tree, an) {
                        ins.push(ie);
                    }
                }
                (oe, ins)
            };
            let mut deps = Vec::new();
            for ie in in_eids {
                self.collect_expr_deps(&arena, ie, &mut deps);
            }
            if let Some((dst_net, dst_range)) = self.resolve_expr_slice(&arena, out_eid) {
                self.add_driver_and_load(
                    dst_net,
                    dst_range,
                    None,
                    None,
                    DriverOrigin::Udp(udp_name.clone()),
                    ExprOpSummary::Comb,
                    deps,
                    Vec::new(),
                    None,
                    None,
                    item.loc,
                );
            }
        }
    }

    pub(crate) fn walk_net_alias(&mut self, inst_id: InstanceId, item: &ElabItem) {
        let t = &self.trees[item.tree];
        let exprs: Vec<NodeId> = t
            .children(item.node)
            .filter(|&c| t.kind(c) != SyntaxKind::Attribute)
            .collect();
        if exprs.len() < 2 {
            return;
        }
        let mut arena = ExprArena::new();
        let eids = {
            let mut binder = ExprBinder::new(
                self.trees,
                self.design,
                inst_id,
                &item.scope_prefix,
                &item.gen_consts,
                &mut arena,
            );
            exprs
                .into_iter()
                .filter_map(|en| binder.bind_self(item.tree, en))
                .collect::<Vec<_>>()
        };
        let mut slices = Vec::new();
        for eid in eids {
            if let Some(sl) = self.resolve_expr_slice(&arena, eid) {
                slices.push(sl);
            }
        }
        if slices.len() >= 2 {
            let (a_net, a_rng) = slices[0];
            for &(b_net, b_rng) in &slices[1..] {
                self.add_driver_and_load(
                    a_net,
                    a_rng,
                    Some(b_net),
                    Some(b_rng),
                    DriverOrigin::NetAlias,
                    ExprOpSummary::Identity,
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    item.loc,
                );
                self.add_driver_and_load(
                    b_net,
                    b_rng,
                    Some(a_net),
                    Some(a_rng),
                    DriverOrigin::NetAlias,
                    ExprOpSummary::Identity,
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    item.loc,
                );
            }
        }
    }

    pub(crate) fn walk_procedural_block(
        &mut self,
        inst_id: InstanceId,
        cid: CellId,
        item: &ElabItem,
    ) {
        let t = &self.trees[item.tree];
        let kw = own_first_tok(t, item.node).unwrap_or("");
        if matches!(kw, "initial" | "final" | "") {
            return;
        }
        let mut is_seq = false;
        let mut is_latch = kw == "always_latch";
        let mut clk_net: Option<NetSegId> = None;
        let mut async_rst: Option<(NetSegId, BitRange, bool)> = None;
        let mut body_node = t
            .children(item.node)
            .find(|&c| t.kind(c) != SyntaxKind::Attribute);

        if let Some(bn) = body_node {
            if t.kind(bn) == SyntaxKind::TimedStmt {
                if let Some(tc) = t.child(bn, SyntaxKind::TimingControl) {
                    let events: Vec<NodeId> = t
                        .children(tc)
                        .filter(|&c| t.kind(c) == SyntaxKind::EventExpr)
                        .collect();
                    let mut parsed_ev = Vec::new();
                    for ev in events {
                        let inner = t.child(ev, SyntaxKind::EventExpr).unwrap_or(ev);
                        let toks = own_toks(t, inner);
                        let is_pos = toks.contains(&"posedge");
                        let is_neg = toks.contains(&"negedge");
                        if is_pos || is_neg {
                            if let Some(sn) = t
                                .children(inner)
                                .find(|&c| t.kind(c) != SyntaxKind::Attribute)
                            {
                                let mut arena = ExprArena::new();
                                let eid_opt = {
                                    let mut binder = ExprBinder::new(
                                        self.trees,
                                        self.design,
                                        inst_id,
                                        &item.scope_prefix,
                                        &item.gen_consts,
                                        &mut arena,
                                    );
                                    binder.bind_self(item.tree, sn)
                                };
                                if let Some(eid) = eid_opt {
                                    if let Some((net, range)) = self.resolve_expr_slice(&arena, eid)
                                    {
                                        parsed_ev.push((net, range, is_pos));
                                    }
                                }
                            }
                        }
                    }
                    if !parsed_ev.is_empty() {
                        is_seq = true;
                        clk_net = Some(parsed_ev[0].0);
                        if parsed_ev.len() >= 2 {
                            async_rst = Some(parsed_ev[1]);
                        }
                    }
                }
                body_node = t.children(bn).find(|&c| {
                    !matches!(t.kind(c), SyntaxKind::Attribute | SyntaxKind::TimingControl)
                });
            } else if kw == "always_ff" {
                is_seq = true;
            }
        }

        let Some(body) = body_node else {
            return;
        };
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        self.walk_stmt(
            item.tree,
            body,
            inst_id,
            &item.scope_prefix,
            &item.gen_consts,
            &mut writes,
            &mut reads,
            is_seq,
            is_latch,
        );
        if kw == "always" && !is_seq && is_latch_hint(t, body) {
            is_latch = true;
        }
        let origin = if is_latch {
            DriverOrigin::AlwaysLatch
        } else if is_seq {
            DriverOrigin::AlwaysFf
        } else {
            DriverOrigin::AlwaysComb
        };
        let op = if is_seq || is_latch {
            ExprOpSummary::Seq
        } else {
            ExprOpSummary::Comb
        };
        let mut seen_w = BTreeSet::new();
        for (dst_net, dst_range) in writes {
            if is_seq || is_latch {
                let seg = &mut self.graph.nets[dst_net.0 as usize];
                seg.is_seq_state = true;
                seg.kind = if is_latch {
                    NetSegKind::Latch
                } else {
                    NetSegKind::Reg
                };
                if seen_w.insert((dst_net, dst_range)) {
                    let elem = SeqElement {
                        net: dst_net,
                        range: dst_range,
                        full_path: seg.full_path.clone(),
                        upf_path: seg.upf_path.clone(),
                        is_latch,
                        clock: clk_net,
                        async_reset: async_rst.map(|(n, _, _)| n),
                        async_reset_range: async_rst.map(|(_, r, _)| r),
                        async_reset_active_high: async_rst.map(|(_, _, ah)| ah),
                        loc: item.loc,
                    };
                    if is_latch {
                        self.graph.cells[cid.0 as usize].latches.push(elem);
                    } else {
                        self.graph.cells[cid.0 as usize].flops.push(elem);
                    }
                }
            }
            for &(src_net, src_range) in &reads {
                self.add_driver_and_load(
                    dst_net,
                    dst_range,
                    Some(src_net),
                    Some(src_range),
                    origin.clone(),
                    op.clone(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    item.loc,
                );
            }
            if reads.is_empty() {
                self.graph.nets[dst_net.0 as usize]
                    .drivers
                    .push(DriverEdge {
                        dst_range,
                        src_net: None,
                        src_range: None,
                        origin: origin.clone(),
                        op: op.clone(),
                        extra_deps: Vec::new(),
                        dep_ops: Vec::new(),
                        inline_clamp: None,
                        loc: item.loc,
                    });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_stmt(
        &mut self,
        tree_idx: usize,
        node: NodeId,
        inst_id: InstanceId,
        scope_prefix: &str,
        gen_consts: &[(String, CVal)],
        writes: &mut Vec<(NetSegId, BitRange)>,
        reads: &mut Vec<(NetSegId, BitRange)>,
        is_seq: bool,
        is_latch: bool,
    ) {
        let t = &self.trees[tree_idx];
        match t.kind(node) {
            SyntaxKind::SeqBlock | SyntaxKind::ParBlock | SyntaxKind::ElseClause => {
                let stmts: Vec<NodeId> = t
                    .children(node)
                    .filter(|&ch| {
                        !matches!(
                            t.kind(ch),
                            SyntaxKind::Attribute
                                | SyntaxKind::StmtLabel
                                | SyntaxKind::Name
                                | SyntaxKind::EndLabel
                        )
                    })
                    .collect();
                if t.kind(node) == SyntaxKind::SeqBlock && !is_seq && !is_latch {
                    for pair in stmts.windows(2) {
                        self.try_record_default_if_clamp(
                            tree_idx,
                            pair[0],
                            pair[1],
                            inst_id,
                            scope_prefix,
                            gen_consts,
                        );
                    }
                }
                for ch in stmts {
                    self.walk_stmt(
                        tree_idx,
                        ch,
                        inst_id,
                        scope_prefix,
                        gen_consts,
                        writes,
                        reads,
                        is_seq,
                        is_latch,
                    );
                }
            }
            SyntaxKind::IfStmt
            | SyntaxKind::CaseStmt
            | SyntaxKind::LoopStmt
            | SyntaxKind::DoWhileStmt => {
                if t.kind(node) == SyntaxKind::IfStmt && !is_seq && !is_latch {
                    self.try_record_if_clamp(tree_idx, node, inst_id, scope_prefix, gen_consts);
                }
                for ch in t.children(node) {
                    let k = t.kind(ch);
                    if matches!(
                        k,
                        SyntaxKind::SeqBlock
                            | SyntaxKind::IfStmt
                            | SyntaxKind::ElseClause
                            | SyntaxKind::CaseStmt
                            | SyntaxKind::LoopStmt
                            | SyntaxKind::DoWhileStmt
                            | SyntaxKind::ExprStmt
                            | SyntaxKind::EmptyStmt
                    ) {
                        self.walk_stmt(
                            tree_idx,
                            ch,
                            inst_id,
                            scope_prefix,
                            gen_consts,
                            writes,
                            reads,
                            is_seq,
                            is_latch,
                        );
                    } else if !matches!(k, SyntaxKind::Attribute | SyntaxKind::StmtLabel) {
                        let mut arena = ExprArena::new();
                        let eid_opt = {
                            let mut binder = ExprBinder::new(
                                self.trees,
                                self.design,
                                inst_id,
                                scope_prefix,
                                gen_consts,
                                &mut arena,
                            );
                            binder.bind_self(tree_idx, ch)
                        };
                        if let Some(eid) = eid_opt {
                            self.collect_expr_deps(&arena, eid, reads);
                        }
                    }
                }
            }
            SyntaxKind::ExprStmt => {
                let Some(expr_node) = t
                    .children(node)
                    .find(|&c| !matches!(t.kind(c), SyntaxKind::Attribute | SyntaxKind::StmtLabel))
                else {
                    return;
                };
                if t.kind(expr_node) == SyntaxKind::BinaryExpr {
                    let kids: Vec<NodeId> = t
                        .children(expr_node)
                        .filter(|&c| t.kind(c) != SyntaxKind::Attribute)
                        .collect();
                    if kids.len() == 2 {
                        let mut arena = ExprArena::new();
                        let (le_opt, re_opt) = {
                            let mut binder = ExprBinder::new(
                                self.trees,
                                self.design,
                                inst_id,
                                scope_prefix,
                                gen_consts,
                                &mut arena,
                            );
                            let le = binder.bind_self(tree_idx, kids[0]);
                            let re = le.and_then(|l| {
                                let lty = binder.arena.get(l).ty.clone();
                                binder.bind_assigned(tree_idx, kids[1], &lty)
                            });
                            (le, re)
                        };
                        if let Some(le) = le_opt {
                            self.collect_lhs_targets(&arena, le, writes);
                            if let Some(re) = re_opt {
                                self.collect_expr_deps(&arena, re, reads);
                                if !is_seq && !is_latch {
                                    if let Some((dst_net, dst_range)) =
                                        self.resolve_expr_slice(&arena, le)
                                    {
                                        if let Some(ic) = self.detect_inline_clamp(&arena, re) {
                                            self.graph.nets[dst_net.0 as usize].drivers.push(
                                                DriverEdge {
                                                    dst_range,
                                                    src_net: Some(ic.data_net),
                                                    src_range: Some(ic.data_range),
                                                    origin: DriverOrigin::AlwaysComb,
                                                    op: ExprOpSummary::Comb,
                                                    extra_deps: vec![(ic.ctrl_net, ic.ctrl_range)],
                                                    dep_ops: Vec::new(),
                                                    inline_clamp: Some(ic),
                                                    loc: Loc::default(),
                                                },
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn collect_lhs_targets(
        &self,
        arena: &ExprArena,
        eid: ExprId,
        out: &mut Vec<(NetSegId, BitRange)>,
    ) {
        if let ExprKind::Concat(parts) = &arena.get(eid).kind {
            for &p in parts {
                self.collect_lhs_targets(arena, p, out);
            }
            return;
        }
        if let Some(sl) = self.resolve_expr_slice(arena, eid) {
            out.push(sl);
        }
    }
}

fn own_first_tok(t: &Tree, node: NodeId) -> Option<&str> {
    t.node(node).children.iter().find_map(|c| match *c {
        Child::Token(i) => {
            let tok = &t.tokens[i as usize];
            (!tok.kind.is_trivia()).then_some(&*tok.text)
        }
        _ => None,
    })
}

pub(crate) fn own_toks(t: &Tree, node: NodeId) -> Vec<&str> {
    t.node(node)
        .children
        .iter()
        .filter_map(|c| match *c {
            Child::Token(i) => {
                let tok = &t.tokens[i as usize];
                (!tok.kind.is_trivia()).then_some(&*tok.text)
            }
            _ => None,
        })
        .collect()
}

fn is_latch_hint(t: &Tree, node: NodeId) -> bool {
    t.kind(node) == SyntaxKind::IfStmt && t.child(node, SyntaxKind::ElseClause).is_none()
}
