//! `always_comb` `if/else` and default-assign + `if` override inline clamp recognition.

use svlang::semantic::expr::{ExprArena, ExprBinder};
use svlang::semantic::{CVal, InstanceId};
use svlang::syntax::{NodeId, SyntaxKind, Tree};
use svlang::Loc;

use super::builder::Builder;
use super::stmt_walk::own_toks;
use super::{DriverEdge, DriverOrigin, ExprOpSummary, InlineClampInfo};

impl<'a> Builder<'a> {
    pub(crate) fn try_record_if_clamp(
        &mut self,
        tree_idx: usize,
        if_node: NodeId,
        inst_id: InstanceId,
        scope_prefix: &str,
        gen_consts: &[(String, CVal)],
    ) {
        let t = &self.trees[tree_idx];
        let kids: Vec<NodeId> = t
            .children(if_node)
            .filter(|&c| !matches!(t.kind(c), SyntaxKind::Attribute | SyntaxKind::StmtLabel))
            .collect();
        if kids.len() < 2 {
            return;
        }
        let cond_node = kids[0];
        let then_stmt = kids[1];
        let else_stmt = if let Some(ec) = t.child(if_node, SyntaxKind::ElseClause) {
            t.children(ec).find(|&c| t.kind(c) != SyntaxKind::Attribute)
        } else {
            kids.get(2)
                .copied()
                .filter(|&c| t.kind(c) != SyntaxKind::ElseClause)
        };
        let Some(else_stmt) = else_stmt else {
            return;
        };
        let Some((then_lhs_n, then_rhs_n)) = extract_single_assign(t, then_stmt) else {
            return;
        };
        let Some((else_lhs_n, else_rhs_n)) = extract_single_assign(t, else_stmt) else {
            return;
        };
        self.bind_and_record_mux_clamp(
            tree_idx,
            inst_id,
            scope_prefix,
            gen_consts,
            cond_node,
            then_lhs_n,
            then_rhs_n,
            else_lhs_n,
            else_rhs_n,
        );
    }

    pub(crate) fn try_record_default_if_clamp(
        &mut self,
        tree_idx: usize,
        def_stmt: NodeId,
        if_node: NodeId,
        inst_id: InstanceId,
        scope_prefix: &str,
        gen_consts: &[(String, CVal)],
    ) {
        let t = &self.trees[tree_idx];
        if t.kind(def_stmt) != SyntaxKind::ExprStmt
            || t.kind(if_node) != SyntaxKind::IfStmt
            || t.child(if_node, SyntaxKind::ElseClause).is_some()
        {
            return;
        }
        let kids: Vec<NodeId> = t
            .children(if_node)
            .filter(|&c| !matches!(t.kind(c), SyntaxKind::Attribute | SyntaxKind::StmtLabel))
            .collect();
        if kids.len() != 2 {
            return;
        }
        let Some((def_lhs_n, def_rhs_n)) = extract_single_assign(t, def_stmt) else {
            return;
        };
        let Some((then_lhs_n, then_rhs_n)) = extract_single_assign(t, kids[1]) else {
            return;
        };
        self.bind_and_record_mux_clamp(
            tree_idx,
            inst_id,
            scope_prefix,
            gen_consts,
            kids[0],
            then_lhs_n,
            then_rhs_n,
            def_lhs_n,
            def_rhs_n,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_and_record_mux_clamp(
        &mut self,
        tree_idx: usize,
        inst_id: InstanceId,
        scope_prefix: &str,
        gen_consts: &[(String, CVal)],
        cond_node: NodeId,
        then_lhs_n: NodeId,
        then_rhs_n: NodeId,
        else_lhs_n: NodeId,
        else_rhs_n: NodeId,
    ) {
        let mut arena = ExprArena::new();
        let bound = {
            let mut binder = ExprBinder::new(
                self.trees,
                self.design,
                inst_id,
                scope_prefix,
                gen_consts,
                &mut arena,
            );
            let Some(ce) = binder.bind_self(tree_idx, cond_node) else {
                return;
            };
            let Some(tle) = binder.bind_self(tree_idx, then_lhs_n) else {
                return;
            };
            let tl_ty = binder.arena.get(tle).ty.clone();
            let Some(tre) = binder.bind_assigned(tree_idx, then_rhs_n, &tl_ty) else {
                return;
            };
            let Some(ele) = binder.bind_self(tree_idx, else_lhs_n) else {
                return;
            };
            let el_ty = binder.arena.get(ele).ty.clone();
            let Some(ere) = binder.bind_assigned(tree_idx, else_rhs_n, &el_ty) else {
                return;
            };
            (ce, tle, tre, ele, ere)
        };
        let (cond_eid, then_le, then_re, else_le, else_re) = bound;
        let Some((dst_net, dst_range)) = self.resolve_expr_slice(&arena, then_le) else {
            return;
        };
        let Some(else_dst) = self.resolve_expr_slice(&arena, else_le) else {
            return;
        };
        if (dst_net, dst_range) != else_dst {
            return;
        }
        let Some((ctrl_net, ctrl_range, cond_inv)) = self.strip_cond_negation(&arena, cond_eid)
        else {
            return;
        };
        let ic = if let Some(clamp_one) = self.const_zero_or_all_ones(&arena, then_re) {
            let Some((data_net, data_range)) =
                self.resolve_expr_slice(&arena, self.peel_casts(&arena, else_re))
            else {
                return;
            };
            InlineClampInfo {
                ctrl_net,
                ctrl_range,
                active_high: !cond_inv,
                clamp_one,
                data_net,
                data_range,
            }
        } else if let Some(clamp_one) = self.const_zero_or_all_ones(&arena, else_re) {
            let Some((data_net, data_range)) =
                self.resolve_expr_slice(&arena, self.peel_casts(&arena, then_re))
            else {
                return;
            };
            InlineClampInfo {
                ctrl_net,
                ctrl_range,
                active_high: cond_inv,
                clamp_one,
                data_net,
                data_range,
            }
        } else {
            return;
        };
        self.graph.nets[dst_net.0 as usize]
            .drivers
            .push(DriverEdge {
                dst_range,
                src_net: Some(ic.data_net),
                src_range: Some(ic.data_range),
                origin: DriverOrigin::AlwaysComb,
                op: ExprOpSummary::Comb,
                extra_deps: vec![(ic.ctrl_net, ic.ctrl_range)],
                dep_ops: Vec::new(),
                inline_clamp: Some(ic),
                loc: Loc::default(),
            });
    }
}

fn extract_single_assign(t: &Tree, mut stmt: NodeId) -> Option<(NodeId, NodeId)> {
    while t.kind(stmt) == SyntaxKind::SeqBlock {
        let rest: Vec<NodeId> = t
            .children(stmt)
            .filter(|&c| {
                !matches!(
                    t.kind(c),
                    SyntaxKind::Attribute
                        | SyntaxKind::StmtLabel
                        | SyntaxKind::Name
                        | SyntaxKind::EndLabel
                        | SyntaxKind::DataDecl
                        | SyntaxKind::ParamDecl
                        | SyntaxKind::TypedefDecl
                )
            })
            .collect();
        if rest.len() == 1 {
            stmt = rest[0];
        } else {
            return None;
        }
    }
    if t.kind(stmt) != SyntaxKind::ExprStmt {
        return None;
    }
    let expr_node = t
        .children(stmt)
        .find(|&c| !matches!(t.kind(c), SyntaxKind::Attribute | SyntaxKind::StmtLabel))?;
    if t.kind(expr_node) != SyntaxKind::BinaryExpr {
        return None;
    }
    if own_toks(t, expr_node).first().copied() != Some("=") {
        return None;
    }
    let kids: Vec<NodeId> = t
        .children(expr_node)
        .filter(|&c| t.kind(c) != SyntaxKind::Attribute)
        .collect();
    (kids.len() == 2).then(|| (kids[0], kids[1]))
}
