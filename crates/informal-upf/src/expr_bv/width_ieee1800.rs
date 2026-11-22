//! Deterministic standalone signal width derivation and 2-pass IEEE 1800-2023 §11.6
//! (`self_width` bottom-up + `context_width` top-down) expression width typer.

use std::collections::{BTreeMap, BTreeSet};

use super::ast::{collect_signals_into, CmpOp, LogicConstraintAst, MAX_LITERAL_WIDTH};
use super::{query_provider_width, SignalWidthProvider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedExpr {
    pub self_w: u32,
    pub ctx_w: u32,
    pub kind: TypedExprKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedExprKind {
    LitBool(bool),
    BitConst {
        width: u32,
        bits: Vec<bool>,
    },
    FillBit(bool),
    Signal {
        name: String,
        sig_width: u32,
        msb: Option<u32>,
        lsb: Option<u32>,
    },
    Not(Box<TypedExpr>),
    BitNot(Box<TypedExpr>),
    RedAnd(Box<TypedExpr>),
    RedOr(Box<TypedExpr>),
    RedXor(Box<TypedExpr>),
    And(Vec<TypedExpr>),
    Or(Vec<TypedExpr>),
    BitAnd(Box<TypedExpr>, Box<TypedExpr>),
    BitOr(Box<TypedExpr>, Box<TypedExpr>),
    BitXor(Box<TypedExpr>, Box<TypedExpr>),
    Eq(Box<TypedExpr>, Box<TypedExpr>),
    Neq(Box<TypedExpr>, Box<TypedExpr>),
    Cmp {
        op: CmpOp,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    Shl(Box<TypedExpr>, u32),
    Shr(Box<TypedExpr>, u32),
    Ite {
        cond: Box<TypedExpr>,
        then_br: Box<TypedExpr>,
        else_br: Box<TypedExpr>,
    },
}

/// Derives deterministic signal widths from `provider` when available, or
/// `max(1, max_bit_select(sig) + 1, max_peer_sized_literal(sig))` in standalone mode.
pub fn derive_standalone_signal_widths(
    asts: &[&LogicConstraintAst],
    provider: &dyn SignalWidthProvider,
) -> BTreeMap<String, u32> {
    let mut signals = BTreeSet::new();
    for ast in asts {
        collect_signals_into(ast, &mut signals);
    }

    let mut widths = BTreeMap::new();
    let mut fixed = BTreeMap::new();
    for sig in signals {
        if let Some(w) = query_provider_width(provider, &sig) {
            fixed.insert(sig.clone(), w);
            widths.insert(sig, w);
        } else {
            widths.insert(sig, 1);
        }
    }

    for ast in asts {
        collect_bit_select_widths(ast, &fixed, &mut widths);
    }

    let max_iters = asts.len().saturating_add(widths.len()).saturating_add(4);
    for pass in 0..max_iters {
        let rev_order = pass % 2 == 1;
        let mut changed = false;
        let iter: Box<dyn Iterator<Item = &&LogicConstraintAst>> = if rev_order {
            Box::new(asts.iter().rev())
        } else {
            Box::new(asts.iter())
        };
        for ast in iter {
            let root_w = expr_self_w(ast, &widths);
            if propagate_peer_widths(ast, root_w, &mut widths, &fixed, rev_order) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    widths
}

pub fn typecheck_ast(ast: &LogicConstraintAst, provider: &dyn SignalWidthProvider) -> TypedExpr {
    let widths = derive_standalone_signal_widths(&[ast], provider);
    typecheck_ast_with_widths(ast, &widths)
}

pub fn typecheck_asts(
    asts: &[&LogicConstraintAst],
    provider: &dyn SignalWidthProvider,
) -> (BTreeMap<String, u32>, Vec<TypedExpr>) {
    let widths = derive_standalone_signal_widths(asts, provider);
    let typed = asts
        .iter()
        .map(|ast| typecheck_ast_with_widths(ast, &widths))
        .collect();
    (widths, typed)
}

pub(crate) fn typecheck_ast_with_widths(
    ast: &LogicConstraintAst,
    widths: &BTreeMap<String, u32>,
) -> TypedExpr {
    let self_typed = annotate_self(ast, widths);
    let root_ctx = self_typed.self_w;
    assign_ctx(self_typed, root_ctx)
}

fn collect_bit_select_widths(
    ast: &LogicConstraintAst,
    fixed: &BTreeMap<String, u32>,
    widths: &mut BTreeMap<String, u32>,
) {
    match ast {
        LogicConstraintAst::LitBool(_)
        | LogicConstraintAst::BitConst { .. }
        | LogicConstraintAst::FillBit(_) => {}
        LogicConstraintAst::Signal { name, msb, lsb } => {
            if !fixed.contains_key(name) {
                let mut req = 1u32;
                if let Some(m) = msb {
                    req = req.max(m.saturating_add(1));
                }
                if let Some(l) = lsb {
                    req = req.max(l.saturating_add(1));
                }
                let capped = req.min(MAX_LITERAL_WIDTH);
                if let Some(cur) = widths.get_mut(name) {
                    *cur = (*cur).max(capped);
                }
            }
        }
        LogicConstraintAst::Not(x)
        | LogicConstraintAst::BitNot(x)
        | LogicConstraintAst::RedAnd(x)
        | LogicConstraintAst::RedOr(x)
        | LogicConstraintAst::RedXor(x)
        | LogicConstraintAst::Shl(x, _)
        | LogicConstraintAst::Shr(x, _) => collect_bit_select_widths(x, fixed, widths),
        LogicConstraintAst::And(items) | LogicConstraintAst::Or(items) => {
            for it in items {
                collect_bit_select_widths(it, fixed, widths);
            }
        }
        LogicConstraintAst::BitAnd(a, b)
        | LogicConstraintAst::BitOr(a, b)
        | LogicConstraintAst::BitXor(a, b)
        | LogicConstraintAst::Eq(a, b)
        | LogicConstraintAst::Neq(a, b)
        | LogicConstraintAst::Cmp { lhs: a, rhs: b, .. } => {
            collect_bit_select_widths(a, fixed, widths);
            collect_bit_select_widths(b, fixed, widths);
        }
        LogicConstraintAst::Ite {
            cond,
            then_br,
            else_br,
        } => {
            collect_bit_select_widths(cond, fixed, widths);
            collect_bit_select_widths(then_br, fixed, widths);
            collect_bit_select_widths(else_br, fixed, widths);
        }
    }
}

fn expr_self_w(ast: &LogicConstraintAst, widths: &BTreeMap<String, u32>) -> u32 {
    match ast {
        LogicConstraintAst::LitBool(_)
        | LogicConstraintAst::FillBit(_)
        | LogicConstraintAst::Not(_)
        | LogicConstraintAst::RedAnd(_)
        | LogicConstraintAst::RedOr(_)
        | LogicConstraintAst::RedXor(_)
        | LogicConstraintAst::And(_)
        | LogicConstraintAst::Or(_)
        | LogicConstraintAst::Eq(..)
        | LogicConstraintAst::Neq(..)
        | LogicConstraintAst::Cmp { .. } => 1,
        LogicConstraintAst::BitConst { width, .. } => (*width).clamp(1, MAX_LITERAL_WIDTH),
        LogicConstraintAst::Signal { name, msb, lsb } => match (msb, lsb) {
            (Some(m), Some(l)) => m.abs_diff(*l).saturating_add(1).min(MAX_LITERAL_WIDTH),
            (Some(_), None) | (None, Some(_)) => 1,
            (None, None) => widths.get(name).copied().unwrap_or(1).max(1),
        },
        LogicConstraintAst::BitNot(x)
        | LogicConstraintAst::Shl(x, _)
        | LogicConstraintAst::Shr(x, _) => expr_self_w(x, widths),
        LogicConstraintAst::BitAnd(a, b)
        | LogicConstraintAst::BitOr(a, b)
        | LogicConstraintAst::BitXor(a, b)
        | LogicConstraintAst::Ite {
            then_br: a,
            else_br: b,
            ..
        } => expr_self_w(a, widths).max(expr_self_w(b, widths)),
    }
}

fn propagate_peer_widths(
    ast: &LogicConstraintAst,
    peer_w: u32,
    widths: &mut BTreeMap<String, u32>,
    fixed: &BTreeMap<String, u32>,
    rev_order: bool,
) -> bool {
    let mut changed = false;
    match ast {
        LogicConstraintAst::LitBool(_)
        | LogicConstraintAst::BitConst { .. }
        | LogicConstraintAst::FillBit(_) => {}
        LogicConstraintAst::Signal { name, msb, lsb } => {
            if msb.is_none() && lsb.is_none() && !fixed.contains_key(name) {
                let capped = peer_w.clamp(1, MAX_LITERAL_WIDTH);
                if let Some(cur) = widths.get_mut(name) {
                    if capped > *cur {
                        *cur = capped;
                        changed = true;
                    }
                }
            }
        }
        LogicConstraintAst::Not(x)
        | LogicConstraintAst::RedAnd(x)
        | LogicConstraintAst::RedOr(x)
        | LogicConstraintAst::RedXor(x) => {
            let sw = expr_self_w(x, widths);
            if propagate_peer_widths(x, sw, widths, fixed, rev_order) {
                changed = true;
            }
        }
        LogicConstraintAst::And(items) | LogicConstraintAst::Or(items) => {
            if rev_order {
                for it in items.iter().rev() {
                    let sw = expr_self_w(it, widths);
                    if propagate_peer_widths(it, sw, widths, fixed, rev_order) {
                        changed = true;
                    }
                }
            } else {
                for it in items {
                    let sw = expr_self_w(it, widths);
                    if propagate_peer_widths(it, sw, widths, fixed, rev_order) {
                        changed = true;
                    }
                }
            }
        }
        LogicConstraintAst::Eq(a, b)
        | LogicConstraintAst::Neq(a, b)
        | LogicConstraintAst::Cmp { lhs: a, rhs: b, .. } => {
            let cw = expr_self_w(a, widths).max(expr_self_w(b, widths));
            if rev_order {
                changed |= propagate_peer_widths(b, cw, widths, fixed, rev_order);
                changed |= propagate_peer_widths(a, cw, widths, fixed, rev_order);
            } else {
                changed |= propagate_peer_widths(a, cw, widths, fixed, rev_order);
                changed |= propagate_peer_widths(b, cw, widths, fixed, rev_order);
            }
        }
        LogicConstraintAst::BitNot(x)
        | LogicConstraintAst::Shl(x, _)
        | LogicConstraintAst::Shr(x, _) => {
            let w = peer_w.max(expr_self_w(x, widths));
            changed |= propagate_peer_widths(x, w, widths, fixed, rev_order);
        }
        LogicConstraintAst::BitAnd(a, b)
        | LogicConstraintAst::BitOr(a, b)
        | LogicConstraintAst::BitXor(a, b) => {
            let w = peer_w
                .max(expr_self_w(a, widths))
                .max(expr_self_w(b, widths));
            if rev_order {
                changed |= propagate_peer_widths(b, w, widths, fixed, rev_order);
                changed |= propagate_peer_widths(a, w, widths, fixed, rev_order);
            } else {
                changed |= propagate_peer_widths(a, w, widths, fixed, rev_order);
                changed |= propagate_peer_widths(b, w, widths, fixed, rev_order);
            }
        }
        LogicConstraintAst::Ite {
            cond,
            then_br,
            else_br,
        } => {
            let cw = expr_self_w(cond, widths);
            changed |= propagate_peer_widths(cond, cw, widths, fixed, rev_order);
            let w = peer_w
                .max(expr_self_w(then_br, widths))
                .max(expr_self_w(else_br, widths));
            if rev_order {
                changed |= propagate_peer_widths(else_br, w, widths, fixed, rev_order);
                changed |= propagate_peer_widths(then_br, w, widths, fixed, rev_order);
            } else {
                changed |= propagate_peer_widths(then_br, w, widths, fixed, rev_order);
                changed |= propagate_peer_widths(else_br, w, widths, fixed, rev_order);
            }
        }
    }
    changed
}

fn annotate_self(ast: &LogicConstraintAst, widths: &BTreeMap<String, u32>) -> TypedExpr {
    match ast {
        LogicConstraintAst::LitBool(b) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::LitBool(*b),
        },
        LogicConstraintAst::BitConst { width, bits } => {
            let sw = (*width).clamp(1, MAX_LITERAL_WIDTH);
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::BitConst {
                    width: sw,
                    bits: bits.clone(),
                },
            }
        }
        LogicConstraintAst::FillBit(b) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::FillBit(*b),
        },
        LogicConstraintAst::Signal { name, msb, lsb } => {
            let sig_width = widths.get(name).copied().unwrap_or(1).max(1);
            let self_w = match (msb, lsb) {
                (Some(m), Some(l)) => m.abs_diff(*l).saturating_add(1).min(MAX_LITERAL_WIDTH),
                (Some(_), None) | (None, Some(_)) => 1,
                (None, None) => sig_width,
            };
            TypedExpr {
                self_w,
                ctx_w: self_w,
                kind: TypedExprKind::Signal {
                    name: name.clone(),
                    sig_width,
                    msb: *msb,
                    lsb: *lsb,
                },
            }
        }
        LogicConstraintAst::Not(x) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::Not(Box::new(annotate_self(x, widths))),
        },
        LogicConstraintAst::BitNot(x) => {
            let tx = annotate_self(x, widths);
            let sw = tx.self_w;
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::BitNot(Box::new(tx)),
            }
        }
        LogicConstraintAst::RedAnd(x) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::RedAnd(Box::new(annotate_self(x, widths))),
        },
        LogicConstraintAst::RedOr(x) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::RedOr(Box::new(annotate_self(x, widths))),
        },
        LogicConstraintAst::RedXor(x) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::RedXor(Box::new(annotate_self(x, widths))),
        },
        LogicConstraintAst::And(items) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::And(items.iter().map(|it| annotate_self(it, widths)).collect()),
        },
        LogicConstraintAst::Or(items) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::Or(items.iter().map(|it| annotate_self(it, widths)).collect()),
        },
        LogicConstraintAst::BitAnd(a, b) => {
            let ta = annotate_self(a, widths);
            let tb = annotate_self(b, widths);
            let sw = ta.self_w.max(tb.self_w);
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::BitAnd(Box::new(ta), Box::new(tb)),
            }
        }
        LogicConstraintAst::BitOr(a, b) => {
            let ta = annotate_self(a, widths);
            let tb = annotate_self(b, widths);
            let sw = ta.self_w.max(tb.self_w);
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::BitOr(Box::new(ta), Box::new(tb)),
            }
        }
        LogicConstraintAst::BitXor(a, b) => {
            let ta = annotate_self(a, widths);
            let tb = annotate_self(b, widths);
            let sw = ta.self_w.max(tb.self_w);
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::BitXor(Box::new(ta), Box::new(tb)),
            }
        }
        LogicConstraintAst::Eq(a, b) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::Eq(
                Box::new(annotate_self(a, widths)),
                Box::new(annotate_self(b, widths)),
            ),
        },
        LogicConstraintAst::Neq(a, b) => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::Neq(
                Box::new(annotate_self(a, widths)),
                Box::new(annotate_self(b, widths)),
            ),
        },
        LogicConstraintAst::Cmp { op, lhs, rhs } => TypedExpr {
            self_w: 1,
            ctx_w: 1,
            kind: TypedExprKind::Cmp {
                op: *op,
                lhs: Box::new(annotate_self(lhs, widths)),
                rhs: Box::new(annotate_self(rhs, widths)),
            },
        },
        LogicConstraintAst::Shl(x, amt) => {
            let tx = annotate_self(x, widths);
            let sw = tx.self_w;
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::Shl(Box::new(tx), *amt),
            }
        }
        LogicConstraintAst::Shr(x, amt) => {
            let tx = annotate_self(x, widths);
            let sw = tx.self_w;
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::Shr(Box::new(tx), *amt),
            }
        }
        LogicConstraintAst::Ite {
            cond,
            then_br,
            else_br,
        } => {
            let tc = annotate_self(cond, widths);
            let tt = annotate_self(then_br, widths);
            let te = annotate_self(else_br, widths);
            let sw = tt.self_w.max(te.self_w);
            TypedExpr {
                self_w: sw,
                ctx_w: sw,
                kind: TypedExprKind::Ite {
                    cond: Box::new(tc),
                    then_br: Box::new(tt),
                    else_br: Box::new(te),
                },
            }
        }
    }
}

fn assign_ctx(node: TypedExpr, ctx_w: u32) -> TypedExpr {
    let ctx_w = ctx_w.clamp(1, MAX_LITERAL_WIDTH);
    let self_w = node.self_w;
    let kind = match node.kind {
        TypedExprKind::LitBool(b) => TypedExprKind::LitBool(b),
        TypedExprKind::BitConst { width, bits } => TypedExprKind::BitConst { width, bits },
        TypedExprKind::FillBit(b) => TypedExprKind::FillBit(b),
        TypedExprKind::Signal {
            name,
            sig_width,
            msb,
            lsb,
        } => TypedExprKind::Signal {
            name,
            sig_width,
            msb,
            lsb,
        },
        TypedExprKind::Not(x) => {
            let x_w = x.self_w;
            TypedExprKind::Not(Box::new(assign_ctx(*x, x_w)))
        }
        TypedExprKind::BitNot(x) => TypedExprKind::BitNot(Box::new(assign_ctx(*x, ctx_w))),
        TypedExprKind::RedAnd(x) => {
            let x_w = x.self_w;
            TypedExprKind::RedAnd(Box::new(assign_ctx(*x, x_w)))
        }
        TypedExprKind::RedOr(x) => {
            let x_w = x.self_w;
            TypedExprKind::RedOr(Box::new(assign_ctx(*x, x_w)))
        }
        TypedExprKind::RedXor(x) => {
            let x_w = x.self_w;
            TypedExprKind::RedXor(Box::new(assign_ctx(*x, x_w)))
        }
        TypedExprKind::And(items) => TypedExprKind::And(
            items
                .into_iter()
                .map(|it| {
                    let w = it.self_w;
                    assign_ctx(it, w)
                })
                .collect(),
        ),
        TypedExprKind::Or(items) => TypedExprKind::Or(
            items
                .into_iter()
                .map(|it| {
                    let w = it.self_w;
                    assign_ctx(it, w)
                })
                .collect(),
        ),
        TypedExprKind::BitAnd(a, b) => TypedExprKind::BitAnd(
            Box::new(assign_ctx(*a, ctx_w)),
            Box::new(assign_ctx(*b, ctx_w)),
        ),
        TypedExprKind::BitOr(a, b) => TypedExprKind::BitOr(
            Box::new(assign_ctx(*a, ctx_w)),
            Box::new(assign_ctx(*b, ctx_w)),
        ),
        TypedExprKind::BitXor(a, b) => TypedExprKind::BitXor(
            Box::new(assign_ctx(*a, ctx_w)),
            Box::new(assign_ctx(*b, ctx_w)),
        ),
        TypedExprKind::Eq(a, b) => {
            let cmp_w = a.self_w.max(b.self_w);
            TypedExprKind::Eq(
                Box::new(assign_ctx(*a, cmp_w)),
                Box::new(assign_ctx(*b, cmp_w)),
            )
        }
        TypedExprKind::Neq(a, b) => {
            let cmp_w = a.self_w.max(b.self_w);
            TypedExprKind::Neq(
                Box::new(assign_ctx(*a, cmp_w)),
                Box::new(assign_ctx(*b, cmp_w)),
            )
        }
        TypedExprKind::Cmp { op, lhs, rhs } => {
            let cmp_w = lhs.self_w.max(rhs.self_w);
            TypedExprKind::Cmp {
                op,
                lhs: Box::new(assign_ctx(*lhs, cmp_w)),
                rhs: Box::new(assign_ctx(*rhs, cmp_w)),
            }
        }
        TypedExprKind::Shl(x, amt) => TypedExprKind::Shl(Box::new(assign_ctx(*x, ctx_w)), amt),
        TypedExprKind::Shr(x, amt) => TypedExprKind::Shr(Box::new(assign_ctx(*x, ctx_w)), amt),
        TypedExprKind::Ite {
            cond,
            then_br,
            else_br,
        } => {
            let c_w = cond.self_w;
            TypedExprKind::Ite {
                cond: Box::new(assign_ctx(*cond, c_w)),
                then_br: Box::new(assign_ctx(*then_br, ctx_w)),
                else_br: Box::new(assign_ctx(*else_br, ctx_w)),
            }
        }
    };
    TypedExpr {
        self_w,
        ctx_w,
        kind,
    }
}
