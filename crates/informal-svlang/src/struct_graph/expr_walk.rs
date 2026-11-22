//! Expression slicing, dependency collection, and inline clamp recognition
//! for `svlang::ExprArena`.

use svlang::semantic::expr::{map_bit_offset, BinaryOp, ExprArena, ExprId, ExprKind, UnaryOp};
use svlang::Loc;

use super::builder::Builder;
use super::{BitRange, DriverOrigin, ExprOpSummary, InlineClampInfo, NetSegId, PinConnBinding};

impl<'a> Builder<'a> {
    pub(crate) fn decompose_pin_bindings(
        &self,
        arena: &ExprArena,
        eid: ExprId,
        pin_range: BitRange,
        loc: Loc,
    ) -> Vec<PinConnBinding> {
        let e = arena.get(eid);
        match &e.kind {
            ExprKind::Resize { arg, .. }
            | ExprKind::CastSign { arg, .. }
            | ExprKind::CastFourState { arg, .. } => {
                return self.decompose_pin_bindings(arena, *arg, pin_range, loc);
            }
            ExprKind::Concat(parts) => {
                let mut out = Vec::new();
                let mut hi_cursor = pin_range.hi + 1;
                for &part in parts {
                    let pw = arena.get(part).ty.bit_width().unwrap_or(1).max(1);
                    if hi_cursor <= pin_range.lo {
                        break;
                    }
                    let take = pw.min(hi_cursor - pin_range.lo);
                    let lo = hi_cursor - take;
                    let sub_range = BitRange {
                        lo,
                        hi: hi_cursor - 1,
                    };
                    out.extend(self.decompose_pin_bindings(arena, part, sub_range, loc));
                    hi_cursor = lo;
                }
                return out;
            }
            ExprKind::Replicate { count, expr } if *count > 0 => {
                let mut out = Vec::new();
                let ew = arena.get(*expr).ty.bit_width().unwrap_or(1).max(1);
                let mut hi_cursor = pin_range.hi + 1;
                for _ in 0..*count {
                    if hi_cursor <= pin_range.lo {
                        break;
                    }
                    let take = ew.min(hi_cursor - pin_range.lo);
                    let lo = hi_cursor - take;
                    let sub = BitRange {
                        lo,
                        hi: hi_cursor - 1,
                    };
                    out.extend(self.decompose_pin_bindings(arena, *expr, sub, loc));
                    hi_cursor = lo;
                }
                return out;
            }
            _ => {}
        }

        if let Some((net, range)) = self.resolve_expr_slice(arena, eid) {
            return vec![PinConnBinding {
                pin_range,
                parent_net: Some(net),
                parent_range: range,
                expr_op: ExprOpSummary::Identity,
                expr_deps: vec![(net, range)],
                dep_ops: vec![ExprOpSummary::Identity],
                inline_clamp: None,
                loc,
            }];
        }

        let inline_clamp = self.detect_inline_clamp(arena, eid);
        let mut deps_with_ops = Vec::new();
        self.collect_expr_deps_with_parity(arena, eid, Some(false), &mut deps_with_ops);
        let deps: Vec<(NetSegId, BitRange)> =
            deps_with_ops.iter().map(|&(n, r, _)| (n, r)).collect();
        let dep_ops: Vec<ExprOpSummary> =
            deps_with_ops.iter().map(|(_, _, op)| op.clone()).collect();
        let op = self.summarize_expr_op(arena, eid);
        vec![PinConnBinding {
            pin_range,
            parent_net: None,
            parent_range: BitRange::full(1),
            expr_op: op,
            expr_deps: deps,
            dep_ops,
            inline_clamp,
            loc,
        }]
    }

    pub(crate) fn resolve_expr_slice(
        &self,
        arena: &ExprArena,
        eid: ExprId,
    ) -> Option<(NetSegId, BitRange)> {
        let e = arena.get(eid);
        match &e.kind {
            ExprKind::Symbol(sid) => {
                let nid = *self.graph.sym_to_net.get(sid)?;
                Some((nid, BitRange::full(self.graph.nets[nid.0 as usize].width)))
            }
            ExprKind::Member {
                base,
                field,
                packed_slice,
            } => {
                if let Some((lo, w)) = *packed_slice {
                    let (nid, br) = self.resolve_expr_slice(arena, *base)?;
                    let abs_lo = br.lo + lo;
                    return Some((
                        nid,
                        BitRange {
                            lo: abs_lo,
                            hi: abs_lo + w.saturating_sub(1),
                        },
                    ));
                }
                if let ExprKind::Symbol(sid) = arena.get(*base).kind {
                    if let Some(&nid) = self.unpacked_field_nets.get(&(sid, field.clone())) {
                        return Some((nid, BitRange::full(self.graph.nets[nid.0 as usize].width)));
                    }
                }
                None
            }
            ExprKind::BitSelect {
                base,
                index,
                bounds,
                elem_width,
            } => {
                let (nid, br) = self.resolve_expr_slice(arena, *base)?;
                let icv = arena.get(*index).cval.as_ref()?;
                if !icv.known() {
                    return None;
                }
                let off = map_bit_offset(icv.i(), *bounds, *elem_width, br.width())?;
                let abs_lo = br.lo + off;
                Some((
                    nid,
                    BitRange {
                        lo: abs_lo,
                        hi: abs_lo + elem_width.saturating_sub(1),
                    },
                ))
            }
            ExprKind::PartSelect {
                base,
                msb,
                lsb,
                bounds,
                elem_width,
            } => {
                let (nid, br) = self.resolve_expr_slice(arena, *base)?;
                let low_idx = if bounds.0 >= bounds.1 {
                    (*msb).min(*lsb)
                } else {
                    (*msb).max(*lsb)
                };
                let count = (((*msb - *lsb).unsigned_abs() as u32) + 1) * (*elem_width);
                let off = map_bit_offset(low_idx, *bounds, *elem_width, br.width())?;
                let abs_lo = br.lo + off;
                Some((
                    nid,
                    BitRange {
                        lo: abs_lo,
                        hi: abs_lo + count.saturating_sub(1),
                    },
                ))
            }
            ExprKind::IndexedPartSelect {
                base,
                base_idx,
                width,
                ascending,
                bounds,
                elem_width,
            } => {
                let (nid, br) = self.resolve_expr_slice(arena, *base)?;
                let icv = arena.get(*base_idx).cval.as_ref()?;
                if !icv.known() {
                    return None;
                }
                let span = (width.saturating_sub(1)) as i128;
                let (lo_sv, hi_sv) = if *ascending {
                    (icv.i(), icv.i() + span)
                } else {
                    (icv.i() - span, icv.i())
                };
                let phys_idx = if bounds.0 >= bounds.1 { lo_sv } else { hi_sv };
                let off = map_bit_offset(phys_idx, *bounds, *elem_width, br.width())?;
                let abs_lo = br.lo + off;
                let total = (*width) * (*elem_width);
                Some((
                    nid,
                    BitRange {
                        lo: abs_lo,
                        hi: abs_lo + total.saturating_sub(1),
                    },
                ))
            }
            ExprKind::ArrayIndex { base, index, .. } => {
                if let ExprKind::Symbol(sid) = arena.get(*base).kind {
                    let icv = arena.get(*index).cval.as_ref()?;
                    if icv.known() {
                        if let Some(&nid) = self.unpacked_elem_nets.get(&(sid, icv.i())) {
                            return Some((
                                nid,
                                BitRange::full(self.graph.nets[nid.0 as usize].width),
                            ));
                        }
                    }
                    if let Some(&nid) = self.graph.sym_to_net.get(&sid) {
                        return Some((nid, BitRange::full(self.graph.nets[nid.0 as usize].width)));
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub(crate) fn collect_expr_deps(
        &self,
        arena: &ExprArena,
        eid: ExprId,
        out: &mut Vec<(NetSegId, BitRange)>,
    ) {
        let mut with_ops = Vec::new();
        self.collect_expr_deps_with_parity(arena, eid, Some(false), &mut with_ops);
        for (n, r, _) in with_ops {
            if !out.contains(&(n, r)) {
                out.push((n, r));
            }
        }
    }

    pub(crate) fn collect_expr_deps_with_parity(
        &self,
        arena: &ExprArena,
        eid: ExprId,
        cur_inv: Option<bool>,
        out: &mut Vec<(NetSegId, BitRange, ExprOpSummary)>,
    ) {
        if let Some((n, r)) = self.resolve_expr_slice(arena, eid) {
            let op = match cur_inv {
                Some(false) => ExprOpSummary::Identity,
                Some(true) => ExprOpSummary::Invert,
                None => ExprOpSummary::Comb,
            };
            if !out
                .iter()
                .any(|(on, or, oop)| *on == n && *or == r && *oop == op)
            {
                out.push((n, r, op));
            }
            return;
        }
        match &arena.get(eid).kind {
            ExprKind::Symbol(_)
            | ExprKind::Local(_)
            | ExprKind::Const(_)
            | ExprKind::UnbasedUnsized(_) => {}
            ExprKind::Unary { op, arg } => {
                let next_inv = match op {
                    UnaryOp::Plus | UnaryOp::RedOr | UnaryOp::RedAnd => cur_inv,
                    UnaryOp::BitNot | UnaryOp::LogNot | UnaryOp::RedNor | UnaryOp::RedNand => {
                        cur_inv.map(|b| !b)
                    }
                    _ => None,
                };
                self.collect_expr_deps_with_parity(arena, *arg, next_inv, out);
            }
            ExprKind::Resize { arg, .. }
            | ExprKind::CastSign { arg, .. }
            | ExprKind::CastFourState { arg, .. }
            | ExprKind::Replicate { expr: arg, .. }
            | ExprKind::Sampled { expr: arg, .. } => {
                self.collect_expr_deps_with_parity(arena, *arg, cur_inv, out);
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinaryOp::BitAnd | BinaryOp::LogAnd | BinaryOp::BitOr | BinaryOp::LogOr => {
                    self.collect_expr_deps_with_parity(arena, *lhs, cur_inv, out);
                    self.collect_expr_deps_with_parity(arena, *rhs, cur_inv, out);
                }
                BinaryOp::Eq | BinaryOp::CaseEq | BinaryOp::Neq | BinaryOp::CaseNeq => {
                    let is_eq = matches!(op, BinaryOp::Eq | BinaryOp::CaseEq);
                    if let Some(is_one) = self.const_zero_or_all_ones(arena, *rhs) {
                        let next_inv = cur_inv.map(|b| b ^ (is_eq != is_one));
                        self.collect_expr_deps_with_parity(arena, *lhs, next_inv, out);
                    } else if let Some(is_one) = self.const_zero_or_all_ones(arena, *lhs) {
                        let next_inv = cur_inv.map(|b| b ^ (is_eq != is_one));
                        self.collect_expr_deps_with_parity(arena, *rhs, next_inv, out);
                    } else {
                        self.collect_expr_deps_with_parity(arena, *lhs, None, out);
                        self.collect_expr_deps_with_parity(arena, *rhs, None, out);
                    }
                }
                BinaryOp::BitXor | BinaryOp::BitXnor => {
                    let is_xnor = matches!(op, BinaryOp::BitXnor);
                    if let Some(is_one) = self.const_zero_or_all_ones(arena, *rhs) {
                        let flip = is_xnor ^ is_one;
                        let next_inv = cur_inv.map(|b| b ^ flip);
                        self.collect_expr_deps_with_parity(arena, *lhs, next_inv, out);
                    } else if let Some(is_one) = self.const_zero_or_all_ones(arena, *lhs) {
                        let flip = is_xnor ^ is_one;
                        let next_inv = cur_inv.map(|b| b ^ flip);
                        self.collect_expr_deps_with_parity(arena, *rhs, next_inv, out);
                    } else {
                        self.collect_expr_deps_with_parity(arena, *lhs, None, out);
                        self.collect_expr_deps_with_parity(arena, *rhs, None, out);
                    }
                }
                _ => {
                    self.collect_expr_deps_with_parity(arena, *lhs, None, out);
                    self.collect_expr_deps_with_parity(arena, *rhs, None, out);
                }
            },
            ExprKind::Cond {
                cond,
                then_val,
                else_val,
            } => {
                let t_c = self.const_zero_or_all_ones(arena, *then_val);
                let e_c = self.const_zero_or_all_ones(arena, *else_val);
                match (t_c, e_c) {
                    (Some(true), Some(false)) => {
                        self.collect_expr_deps_with_parity(arena, *cond, cur_inv, out);
                    }
                    (Some(false), Some(true)) => {
                        self.collect_expr_deps_with_parity(arena, *cond, cur_inv.map(|b| !b), out);
                    }
                    _ => {
                        self.collect_expr_deps_with_parity(arena, *cond, None, out);
                        self.collect_expr_deps_with_parity(arena, *then_val, cur_inv, out);
                        self.collect_expr_deps_with_parity(arena, *else_val, cur_inv, out);
                    }
                }
            }
            ExprKind::Concat(parts) if parts.len() == 1 => {
                self.collect_expr_deps_with_parity(arena, parts[0], cur_inv, out);
            }
            ExprKind::Concat(parts)
            | ExprKind::SysCall { args: parts, .. }
            | ExprKind::FuncCall { args: parts, .. } => {
                for &p in parts {
                    self.collect_expr_deps_with_parity(arena, p, None, out);
                }
            }
            ExprKind::BitSelect { base, index, .. } | ExprKind::ArrayIndex { base, index, .. } => {
                self.collect_expr_deps_with_parity(arena, *base, None, out);
                self.collect_expr_deps_with_parity(arena, *index, None, out);
            }
            ExprKind::PartSelect { base, .. } | ExprKind::Member { base, .. } => {
                self.collect_expr_deps_with_parity(arena, *base, None, out);
            }
            ExprKind::IndexedPartSelect { base, base_idx, .. } => {
                self.collect_expr_deps_with_parity(arena, *base, None, out);
                self.collect_expr_deps_with_parity(arena, *base_idx, None, out);
            }
            ExprKind::Inside { expr, .. } => {
                self.collect_expr_deps_with_parity(arena, *expr, None, out);
            }
            ExprKind::AssignPattern(items) => {
                for &(_, e) in items {
                    self.collect_expr_deps_with_parity(arena, e, None, out);
                }
            }
        }
    }

    pub(crate) fn peel_casts(&self, arena: &ExprArena, mut eid: ExprId) -> ExprId {
        loop {
            match arena.get(eid).kind {
                ExprKind::Resize { arg, .. }
                | ExprKind::CastSign { arg, .. }
                | ExprKind::CastFourState { arg, .. } => eid = arg,
                _ => return eid,
            }
        }
    }

    pub(crate) fn emit_rhs_drivers(
        &mut self,
        arena: &ExprArena,
        dst_net: NetSegId,
        dst_range: BitRange,
        rhs_eid: ExprId,
        origin: DriverOrigin,
        loc: Loc,
    ) {
        let peeled = self.peel_casts(arena, rhs_eid);
        if let ExprKind::Cond {
            cond,
            then_val,
            else_val,
        } = &arena.get(peeled).kind
        {
            let t_c = self.const_zero_or_all_ones(arena, *then_val);
            let e_c = self.const_zero_or_all_ones(arena, *else_val);
            if matches!(
                (t_c, e_c),
                (Some(true), Some(false)) | (Some(false), Some(true))
            ) {
                if let Some((cn, cr, cinv)) = self.strip_cond_negation(arena, *cond) {
                    let is_pos = (t_c == Some(true)) ^ cinv;
                    let op = if is_pos {
                        ExprOpSummary::Identity
                    } else {
                        ExprOpSummary::Invert
                    };
                    let load_op = self.summarize_expr_op(arena, peeled);
                    self.add_driver_and_load_with_load_op(
                        dst_net,
                        dst_range,
                        Some(cn),
                        Some(cr),
                        origin,
                        op,
                        Some(load_op),
                        Vec::new(),
                        Vec::new(),
                        None,
                        None,
                        loc,
                    );
                    return;
                }
            }
            let inline_clamp = self.detect_inline_clamp(arena, peeled);
            let sel = self.resolve_expr_slice(arena, self.peel_casts(arena, *cond));
            let mux_op = ExprOpSummary::Mux {
                sel,
                sel_const_true: None,
            };
            let mut cond_deps = Vec::new();
            self.collect_expr_deps(arena, *cond, &mut cond_deps);
            if !cond_deps.is_empty() {
                let cond_dep_ops = vec![ExprOpSummary::Comb; cond_deps.len()];
                self.add_driver_and_load(
                    dst_net,
                    dst_range,
                    sel.map(|s| s.0),
                    sel.map(|s| s.1),
                    origin.clone(),
                    mux_op.clone(),
                    cond_deps,
                    cond_dep_ops,
                    inline_clamp,
                    None,
                    loc,
                );
            }
            for &branch_eid in &[*then_val, *else_val] {
                self.emit_single_branch_driver(
                    arena,
                    dst_net,
                    dst_range,
                    branch_eid,
                    origin.clone(),
                    inline_clamp,
                    Some(&mux_op),
                    loc,
                );
            }
            return;
        }
        self.emit_single_branch_driver(arena, dst_net, dst_range, rhs_eid, origin, None, None, loc);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_single_branch_driver(
        &mut self,
        arena: &ExprArena,
        dst_net: NetSegId,
        dst_range: BitRange,
        rhs_eid: ExprId,
        origin: DriverOrigin,
        inline_clamp: Option<InlineClampInfo>,
        load_op_override: Option<&ExprOpSummary>,
        loc: Loc,
    ) {
        let peeled = self.peel_casts(arena, rhs_eid);
        if let Some((sn, sr, inv)) = self.strip_cond_negation(arena, peeled) {
            let drv_op = if inv {
                ExprOpSummary::Invert
            } else {
                ExprOpSummary::Identity
            };
            let load_op = load_op_override
                .cloned()
                .unwrap_or_else(|| self.summarize_expr_op(arena, peeled));
            self.add_driver_and_load_with_load_op(
                dst_net,
                dst_range,
                Some(sn),
                Some(sr),
                origin,
                drv_op,
                Some(load_op),
                Vec::new(),
                Vec::new(),
                inline_clamp,
                None,
                loc,
            );
            return;
        }
        let clamp = inline_clamp.or_else(|| self.detect_inline_clamp(arena, peeled));
        let mut deps_with_ops = Vec::new();
        self.collect_expr_deps_with_parity(arena, peeled, Some(false), &mut deps_with_ops);
        let deps: Vec<(NetSegId, BitRange)> =
            deps_with_ops.iter().map(|&(n, r, _)| (n, r)).collect();
        let dep_ops: Vec<ExprOpSummary> =
            deps_with_ops.iter().map(|(_, _, op)| op.clone()).collect();
        let drv_op = if dep_ops.len() == 1 {
            dep_ops[0].clone()
        } else {
            self.summarize_expr_op(arena, peeled)
        };
        let load_op = load_op_override
            .cloned()
            .unwrap_or_else(|| self.summarize_expr_op(arena, peeled));
        let primary = (deps.len() == 1).then(|| deps[0]);
        self.add_driver_and_load_with_load_op(
            dst_net,
            dst_range,
            primary.map(|p| p.0),
            primary.map(|p| p.1),
            origin,
            drv_op,
            Some(load_op),
            deps,
            dep_ops,
            clamp,
            None,
            loc,
        );
    }
}
