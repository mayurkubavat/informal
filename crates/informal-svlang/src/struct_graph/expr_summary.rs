//! Expression width, negation, inline-clamp detection, and operator summary helpers
//! for `svlang::ExprArena`.

use svlang::semantic::expr::{BinaryOp, ExprArena, ExprId, ExprKind, UnaryOp};
use svlang::semantic::BitState;

use super::builder::Builder;
use super::{BitRange, ExprOpSummary, InlineClampInfo, NetSegId};

impl<'a> Builder<'a> {
    pub(crate) fn effective_expr_width(&self, arena: &ExprArena, eid: ExprId) -> u32 {
        match &arena.get(eid).kind {
            ExprKind::Resize { arg, width, signed } => {
                if *signed {
                    *width
                } else {
                    self.effective_expr_width(arena, *arg)
                }
            }
            ExprKind::CastSign { arg, .. } | ExprKind::CastFourState { arg, .. } => {
                self.effective_expr_width(arena, *arg)
            }
            ExprKind::Replicate { count, expr } => {
                (*count).saturating_mul(self.effective_expr_width(arena, *expr))
            }
            ExprKind::Unary {
                op: UnaryOp::Plus | UnaryOp::BitNot | UnaryOp::LogNot,
                arg,
            } => self.effective_expr_width(arena, *arg),
            ExprKind::Concat(parts) => parts
                .iter()
                .map(|&p| self.effective_expr_width(arena, p))
                .sum(),
            ExprKind::Unary {
                op: UnaryOp::RedOr | UnaryOp::RedAnd | UnaryOp::RedNor | UnaryOp::RedNand,
                ..
            }
            | ExprKind::Binary {
                op: BinaryOp::Eq | BinaryOp::CaseEq | BinaryOp::Neq | BinaryOp::CaseNeq,
                ..
            } => 1,
            _ => {
                if let Some((_, r)) = self.resolve_expr_slice(arena, eid) {
                    r.width()
                } else {
                    arena.get(eid).ty.bit_width().unwrap_or(1)
                }
            }
        }
    }

    pub(crate) fn const_zero_or_all_ones(&self, arena: &ExprArena, eid: ExprId) -> Option<bool> {
        let e = arena.get(self.peel_casts(arena, eid));
        let cv = e.cval.as_ref()?;
        if !cv.known() {
            return None;
        }
        if !cv.is_true() {
            return Some(false);
        }
        if cv.w == 1 || (0..cv.w).all(|b| cv.bit_state(b) == BitState::One) {
            return Some(true);
        }
        None
    }

    pub(crate) fn strip_cond_negation(
        &self,
        arena: &ExprArena,
        eid: ExprId,
    ) -> Option<(NetSegId, BitRange, bool)> {
        let e = arena.get(self.peel_casts(arena, eid));
        match &e.kind {
            ExprKind::Unary {
                op: UnaryOp::BitNot | UnaryOp::LogNot | UnaryOp::RedNor | UnaryOp::RedNand,
                arg,
            } => {
                let (n, r, inv) = self.strip_cond_negation(arena, *arg)?;
                Some((n, r, !inv))
            }
            ExprKind::Unary {
                op: UnaryOp::Plus | UnaryOp::RedOr | UnaryOp::RedAnd,
                arg,
            } => self.strip_cond_negation(arena, *arg),
            ExprKind::Replicate { expr, .. } => self.strip_cond_negation(arena, *expr),
            ExprKind::Concat(parts) if parts.len() == 1 => {
                self.strip_cond_negation(arena, parts[0])
            }
            ExprKind::Binary {
                op: op @ (BinaryOp::Eq | BinaryOp::CaseEq | BinaryOp::Neq | BinaryOp::CaseNeq),
                lhs,
                rhs,
            } => {
                let is_eq = matches!(op, BinaryOp::Eq | BinaryOp::CaseEq);
                if let Some(is_one) = self.const_zero_or_all_ones(arena, *rhs) {
                    let (n, r, inv) = self.strip_cond_negation(arena, *lhs)?;
                    return Some((n, r, inv ^ (if is_eq { !is_one } else { is_one })));
                }
                if let Some(is_one) = self.const_zero_or_all_ones(arena, *lhs) {
                    let (n, r, inv) = self.strip_cond_negation(arena, *rhs)?;
                    return Some((n, r, inv ^ (if is_eq { !is_one } else { is_one })));
                }
                None
            }
            _ => self
                .resolve_expr_slice(arena, e.id)
                .map(|(n, r)| (n, r, false)),
        }
    }

    pub(crate) fn detect_inline_clamp(
        &self,
        arena: &ExprArena,
        eid: ExprId,
    ) -> Option<InlineClampInfo> {
        let e = arena.get(self.peel_casts(arena, eid));
        match &e.kind {
            ExprKind::Cond {
                cond,
                then_val,
                else_val,
            } => {
                let (ctrl_net, ctrl_range, cond_inv) = self.strip_cond_negation(arena, *cond)?;
                if let Some(clamp_one) = self.const_zero_or_all_ones(arena, *then_val) {
                    if let Some((data_net, data_range)) =
                        self.resolve_expr_slice(arena, self.peel_casts(arena, *else_val))
                    {
                        return Some(InlineClampInfo {
                            ctrl_net,
                            ctrl_range,
                            active_high: !cond_inv,
                            clamp_one,
                            data_net,
                            data_range,
                        });
                    }
                }
                if let Some(clamp_one) = self.const_zero_or_all_ones(arena, *else_val) {
                    if let Some((data_net, data_range)) =
                        self.resolve_expr_slice(arena, self.peel_casts(arena, *then_val))
                    {
                        return Some(InlineClampInfo {
                            ctrl_net,
                            ctrl_range,
                            active_high: cond_inv,
                            clamp_one,
                            data_net,
                            data_range,
                        });
                    }
                }
                None
            }
            ExprKind::Binary {
                op: BinaryOp::BitAnd | BinaryOp::LogAnd | BinaryOp::BitOr | BinaryOp::LogOr,
                lhs,
                rhs,
            } => {
                let is_and = matches!(
                    e.kind,
                    ExprKind::Binary {
                        op: BinaryOp::BitAnd | BinaryOp::LogAnd,
                        ..
                    }
                );
                let l_ctrl = self.strip_cond_negation(arena, *lhs);
                let r_ctrl = self.strip_cond_negation(arena, *rhs);
                let l_pw = self.effective_expr_width(arena, *lhs);
                let r_pw = self.effective_expr_width(arena, *rhs);
                let is_logical = matches!(
                    e.kind,
                    ExprKind::Binary {
                        op: BinaryOp::LogAnd | BinaryOp::LogOr,
                        ..
                    }
                );
                let pick_left = match (&l_ctrl, &r_ctrl) {
                    (Some((_, _, li)), Some((_, _, ri))) => {
                        if l_pw == 1 && r_pw > 1 {
                            true
                        } else if r_pw == 1 && l_pw > 1 {
                            false
                        } else {
                            *li && !*ri
                        }
                    }
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (None, None) => return None,
                };
                let (ctrl_net, ctrl_range, cond_inv, ctrl_pw, data_eid, data_pw) = if pick_left {
                    let (cn, cr, ci) = l_ctrl?;
                    (cn, cr, ci, l_pw, *rhs, r_pw)
                } else {
                    let (cn, cr, ci) = r_ctrl?;
                    (cn, cr, ci, r_pw, *lhs, l_pw)
                };
                if is_logical && data_pw > 1 {
                    return None;
                }
                if ctrl_pw < data_pw {
                    return None;
                }
                let (data_net, data_range) =
                    self.resolve_expr_slice(arena, self.peel_casts(arena, data_eid))?;
                let active_high = if is_and { cond_inv } else { !cond_inv };
                Some(InlineClampInfo {
                    ctrl_net,
                    ctrl_range,
                    active_high,
                    clamp_one: !is_and,
                    data_net,
                    data_range,
                })
            }
            _ => None,
        }
    }

    pub(crate) fn summarize_expr_op(&self, arena: &ExprArena, eid: ExprId) -> ExprOpSummary {
        let e = arena.get(self.peel_casts(arena, eid));
        match &e.kind {
            ExprKind::Symbol(_) => {
                if self.resolve_expr_slice(arena, e.id).is_some() {
                    ExprOpSummary::Identity
                } else if let Some(cv) = e.cval.as_ref().filter(|c| c.known()) {
                    ExprOpSummary::Const {
                        is_zero: !cv.is_true(),
                        is_all_ones: (0..cv.w).all(|b| cv.bit_state(b) == BitState::One),
                    }
                } else {
                    ExprOpSummary::Comb
                }
            }
            ExprKind::BitSelect { .. }
            | ExprKind::PartSelect { .. }
            | ExprKind::Member { .. }
            | ExprKind::IndexedPartSelect { .. }
            | ExprKind::ArrayIndex { .. } => {
                if self.resolve_expr_slice(arena, e.id).is_some() {
                    ExprOpSummary::Identity
                } else {
                    ExprOpSummary::Comb
                }
            }
            ExprKind::Const(cv) => ExprOpSummary::Const {
                is_zero: cv.known() && !cv.is_true(),
                is_all_ones: cv.known() && (0..cv.w).all(|b| cv.bit_state(b) == BitState::One),
            },
            ExprKind::Unary { op, arg } => match op {
                UnaryOp::Plus => self.summarize_expr_op(arena, *arg),
                UnaryOp::BitNot => invert_op_summary(self.summarize_expr_op(arena, *arg)),
                UnaryOp::LogNot if self.effective_expr_width(arena, *arg) == 1 => {
                    invert_op_summary(self.summarize_expr_op(arena, *arg))
                }
                _ => ExprOpSummary::Comb,
            },
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinaryOp::BitAnd | BinaryOp::BitOr => {
                    let l = self.summarize_expr_op(arena, *lhs);
                    let r = self.summarize_expr_op(arena, *rhs);
                    if self.is_bitwise_unate(arena, *lhs, &l)
                        && self.is_bitwise_unate(arena, *rhs, &r)
                    {
                        ExprOpSummary::UnateGate {
                            is_inverting: false,
                        }
                    } else {
                        ExprOpSummary::Comb
                    }
                }
                BinaryOp::Eq | BinaryOp::CaseEq | BinaryOp::Neq | BinaryOp::CaseNeq => {
                    let is_eq = matches!(op, BinaryOp::Eq | BinaryOp::CaseEq);
                    self.fold_1b_eq_const(arena, *rhs, *lhs, is_eq)
                        .or_else(|| self.fold_1b_eq_const(arena, *lhs, *rhs, is_eq))
                        .unwrap_or(ExprOpSummary::Comb)
                }
                BinaryOp::BitXor => {
                    let l = self.summarize_expr_op(arena, *lhs);
                    let r = self.summarize_expr_op(arena, *rhs);
                    match (&l, &r) {
                        (
                            ExprOpSummary::Const {
                                is_all_ones: true, ..
                            },
                            ExprOpSummary::Identity,
                        )
                        | (
                            ExprOpSummary::Identity,
                            ExprOpSummary::Const {
                                is_all_ones: true, ..
                            },
                        ) => ExprOpSummary::Invert,
                        _ => ExprOpSummary::Comb,
                    }
                }
                _ => ExprOpSummary::Comb,
            },
            ExprKind::Cond {
                cond,
                then_val,
                else_val,
            } => {
                let sel = self.resolve_expr_slice(arena, self.peel_casts(arena, *cond));
                let t_c = self.const_zero_or_all_ones(arena, *then_val);
                let e_c = self.const_zero_or_all_ones(arena, *else_val);
                let sel_const_true = match (t_c, e_c) {
                    (Some(true), Some(false)) => Some(true),
                    (Some(false), Some(true)) => Some(false),
                    _ => None,
                };
                ExprOpSummary::Mux {
                    sel,
                    sel_const_true,
                }
            }
            ExprKind::Concat(parts) if parts.len() == 1 => self.summarize_expr_op(arena, parts[0]),
            _ => ExprOpSummary::Comb,
        }
    }

    fn is_bitwise_unate(&self, arena: &ExprArena, id: ExprId, op: &ExprOpSummary) -> bool {
        match op {
            ExprOpSummary::Identity
            | ExprOpSummary::Const { .. }
            | ExprOpSummary::UnateGate {
                is_inverting: false,
            } => true,
            ExprOpSummary::Invert | ExprOpSummary::UnateGate { is_inverting: true } => {
                self.effective_expr_width(arena, id) == 1
            }
            _ => false,
        }
    }

    fn fold_1b_eq_const(
        &self,
        arena: &ExprArena,
        const_side: ExprId,
        var_side: ExprId,
        is_eq: bool,
    ) -> Option<ExprOpSummary> {
        let is_one = self.const_zero_or_all_ones(arena, const_side)?;
        if self.effective_expr_width(arena, var_side) != 1
            || !matches!(
                self.summarize_expr_op(arena, var_side),
                ExprOpSummary::Identity
            )
        {
            return None;
        }
        Some(if is_eq == is_one {
            ExprOpSummary::Identity
        } else {
            ExprOpSummary::Invert
        })
    }
}

fn invert_op_summary(op: ExprOpSummary) -> ExprOpSummary {
    match op {
        ExprOpSummary::Identity => ExprOpSummary::Invert,
        ExprOpSummary::Invert => ExprOpSummary::UnateGate {
            is_inverting: false,
        },
        ExprOpSummary::UnateGate { is_inverting } => ExprOpSummary::UnateGate {
            is_inverting: !is_inverting,
        },
        ExprOpSummary::Const {
            is_zero,
            is_all_ones,
        } => ExprOpSummary::Const {
            is_zero: is_all_ones,
            is_all_ones: is_zero,
        },
        _ => ExprOpSummary::Comb,
    }
}
