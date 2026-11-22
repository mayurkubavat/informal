//! Tseitin bit-vector bitblaster over deterministic [`TypedExpr`] trees.

use std::collections::BTreeMap;

use informal_sat::Lit;

use super::ast::{CmpOp, LogicConstraintAst, MAX_LITERAL_WIDTH};
use super::width_ieee1800::{typecheck_ast, TypedExpr, TypedExprKind};
use super::SignalWidthProvider;

/// Abstract CNF gate emitter implemented by [`crate::pst::cnf::CnfBuilder`].
pub trait CnfEmitter {
    fn const_true(&self) -> Lit;
    fn alloc_var(&mut self) -> Lit;
    fn tseitin_and(&mut self, lits: &[Lit]) -> Lit;
    fn tseitin_or(&mut self, lits: &[Lit]) -> Lit;
    fn tseitin_xor(&mut self, a: Lit, b: Lit) -> Lit;
    fn tseitin_xnor(&mut self, a: Lit, b: Lit) -> Lit;
    fn tseitin_ite(&mut self, c: Lit, t: Lit, e: Lit) -> Lit;
}

pub fn compile_logic_ast<C: CnfEmitter>(
    ast: &LogicConstraintAst,
    provider: &dyn SignalWidthProvider,
    signal_bits: &mut BTreeMap<String, Vec<Lit>>,
    cnf: &mut C,
) -> Lit {
    let typed = typecheck_ast(ast, provider);
    compile_typed_logic_ast(&typed, signal_bits, cnf)
}

pub fn compile_typed_logic_ast<C: CnfEmitter>(
    typed: &TypedExpr,
    signal_bits: &mut BTreeMap<String, Vec<Lit>>,
    cnf: &mut C,
) -> Lit {
    let bits = compile_typed_bv(typed, signal_bits, cnf);
    bv_truthy(&bits, cnf)
}

pub fn compile_typed_bv<C: CnfEmitter>(
    typed: &TypedExpr,
    signal_bits: &mut BTreeMap<String, Vec<Lit>>,
    cnf: &mut C,
) -> Vec<Lit> {
    let ctx_w = typed.ctx_w.max(1) as usize;
    let c_true = cnf.const_true();
    let c_false = -c_true;

    match &typed.kind {
        TypedExprKind::LitBool(b) => {
            let bit = if *b { c_true } else { c_false };
            zero_extend(vec![bit], ctx_w, c_false)
        }
        TypedExprKind::BitConst { bits, .. } => (0..ctx_w)
            .map(|i| {
                if bits.get(i).copied().unwrap_or(false) {
                    c_true
                } else {
                    c_false
                }
            })
            .collect(),
        TypedExprKind::FillBit(b) => {
            vec![if *b { c_true } else { c_false }; ctx_w]
        }
        TypedExprKind::Signal {
            name,
            sig_width,
            msb,
            lsb,
        } => {
            let sw = (*sig_width).max(1) as usize;
            let vec = signal_bits.entry(name.clone()).or_default();
            while vec.len() < sw {
                vec.push(cnf.alloc_var());
            }
            let bits: Vec<Lit> = match (msb, lsb) {
                (Some(hi), Some(lo)) => {
                    let get_bit = |idx: u32| -> Lit {
                        let i = idx as usize;
                        if i < sw {
                            vec[i]
                        } else {
                            c_false
                        }
                    };
                    let slice_len =
                        hi.abs_diff(*lo).saturating_add(1).min(MAX_LITERAL_WIDTH) as usize;
                    if hi >= lo {
                        (*lo..=*hi).take(slice_len).map(get_bit).collect()
                    } else {
                        (*hi..=*lo).rev().take(slice_len).map(get_bit).collect()
                    }
                }
                (Some(m), None) | (None, Some(m)) => {
                    let i = *m as usize;
                    vec![if i < sw { vec[i] } else { c_false }]
                }
                (None, None) => vec[..sw].to_vec(),
            };
            zero_extend(bits, ctx_w, c_false)
        }
        TypedExprKind::Not(inner) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            let b = -bv_truthy(&v, cnf);
            zero_extend(vec![b], ctx_w, c_false)
        }
        TypedExprKind::BitNot(inner) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            v.into_iter().map(|l| -l).collect()
        }
        TypedExprKind::RedAnd(inner) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            let b = cnf.tseitin_and(&v);
            zero_extend(vec![b], ctx_w, c_false)
        }
        TypedExprKind::RedOr(inner) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            let b = cnf.tseitin_or(&v);
            zero_extend(vec![b], ctx_w, c_false)
        }
        TypedExprKind::RedXor(inner) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            let mut acc = c_false;
            for bit in v {
                acc = cnf.tseitin_xor(acc, bit);
            }
            zero_extend(vec![acc], ctx_w, c_false)
        }
        TypedExprKind::And(items) => {
            let lits: Vec<Lit> = items
                .iter()
                .map(|it| {
                    let v = compile_typed_bv(it, signal_bits, cnf);
                    bv_truthy(&v, cnf)
                })
                .collect();
            zero_extend(vec![cnf.tseitin_and(&lits)], ctx_w, c_false)
        }
        TypedExprKind::Or(items) => {
            let lits: Vec<Lit> = items
                .iter()
                .map(|it| {
                    let v = compile_typed_bv(it, signal_bits, cnf);
                    bv_truthy(&v, cnf)
                })
                .collect();
            zero_extend(vec![cnf.tseitin_or(&lits)], ctx_w, c_false)
        }
        TypedExprKind::BitAnd(a, b) => {
            let va = compile_typed_bv(a, signal_bits, cnf);
            let vb = compile_typed_bv(b, signal_bits, cnf);
            (0..ctx_w)
                .map(|i| cnf.tseitin_and(&[va[i], vb[i]]))
                .collect()
        }
        TypedExprKind::BitOr(a, b) => {
            let va = compile_typed_bv(a, signal_bits, cnf);
            let vb = compile_typed_bv(b, signal_bits, cnf);
            (0..ctx_w)
                .map(|i| cnf.tseitin_or(&[va[i], vb[i]]))
                .collect()
        }
        TypedExprKind::BitXor(a, b) => {
            let va = compile_typed_bv(a, signal_bits, cnf);
            let vb = compile_typed_bv(b, signal_bits, cnf);
            (0..ctx_w).map(|i| cnf.tseitin_xor(va[i], vb[i])).collect()
        }
        TypedExprKind::Eq(a, b) => {
            let eq = encode_eq(a, b, signal_bits, cnf);
            zero_extend(vec![eq], ctx_w, c_false)
        }
        TypedExprKind::Neq(a, b) => {
            let eq = encode_eq(a, b, signal_bits, cnf);
            zero_extend(vec![-eq], ctx_w, c_false)
        }
        TypedExprKind::Cmp { op, lhs, rhs } => {
            let va = compile_typed_bv(lhs, signal_bits, cnf);
            let vb = compile_typed_bv(rhs, signal_bits, cnf);
            let w = va.len().min(vb.len());
            let lt = encode_unsigned_lt(&va[..w], &vb[..w], cnf);
            let eq_bits: Vec<Lit> = (0..w).map(|i| cnf.tseitin_xnor(va[i], vb[i])).collect();
            let eq = cnf.tseitin_and(&eq_bits);
            let res = match op {
                CmpOp::Lt => lt,
                CmpOp::Le => cnf.tseitin_or(&[lt, eq]),
                CmpOp::Gt => -cnf.tseitin_or(&[lt, eq]),
                CmpOp::Ge => -lt,
            };
            zero_extend(vec![res], ctx_w, c_false)
        }
        TypedExprKind::Shl(inner, amt) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            if *amt >= typed.ctx_w {
                return vec![c_false; ctx_w];
            }
            let shift = *amt as usize;
            let mut out = vec![c_false; shift];
            out.extend_from_slice(&v[..ctx_w - shift]);
            out
        }
        TypedExprKind::Shr(inner, amt) => {
            let v = compile_typed_bv(inner, signal_bits, cnf);
            if *amt >= typed.ctx_w {
                return vec![c_false; ctx_w];
            }
            let shift = *amt as usize;
            let mut out = v[shift..].to_vec();
            out.resize(ctx_w, c_false);
            out
        }
        TypedExprKind::Ite {
            cond,
            then_br,
            else_br,
        } => {
            let cb = compile_typed_bv(cond, signal_bits, cnf);
            let c = bv_truthy(&cb, cnf);
            let vt = compile_typed_bv(then_br, signal_bits, cnf);
            let ve = compile_typed_bv(else_br, signal_bits, cnf);
            (0..ctx_w)
                .map(|i| cnf.tseitin_ite(c, vt[i], ve[i]))
                .collect()
        }
    }
}

fn encode_eq<C: CnfEmitter>(
    a: &TypedExpr,
    b: &TypedExpr,
    signal_bits: &mut BTreeMap<String, Vec<Lit>>,
    cnf: &mut C,
) -> Lit {
    let va = compile_typed_bv(a, signal_bits, cnf);
    let vb = compile_typed_bv(b, signal_bits, cnf);
    let w = va.len().min(vb.len());
    let eqs: Vec<Lit> = (0..w).map(|i| cnf.tseitin_xnor(va[i], vb[i])).collect();
    cnf.tseitin_and(&eqs)
}

fn bv_truthy<C: CnfEmitter>(bits: &[Lit], cnf: &mut C) -> Lit {
    if bits.is_empty() {
        -cnf.const_true()
    } else if bits.len() == 1 {
        bits[0]
    } else {
        cnf.tseitin_or(bits)
    }
}

fn zero_extend(mut bits: Vec<Lit>, width: usize, c_false: Lit) -> Vec<Lit> {
    if bits.len() > width {
        bits.truncate(width);
    }
    while bits.len() < width {
        bits.push(c_false);
    }
    bits
}

fn encode_unsigned_lt<C: CnfEmitter>(a: &[Lit], b: &[Lit], cnf: &mut C) -> Lit {
    let mut borrow = -cnf.const_true();
    for i in 0..a.len() {
        let a_lt_b = cnf.tseitin_and(&[-a[i], b[i]]);
        let a_eq_b = cnf.tseitin_xnor(a[i], b[i]);
        let keep = cnf.tseitin_and(&[a_eq_b, borrow]);
        borrow = cnf.tseitin_or(&[a_lt_b, keep]);
    }
    borrow
}
