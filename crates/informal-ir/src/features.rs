//! Structural features of a model, used by the run database and, from
//! phase 3, by the learned engine selector (in the style of Btor2-Select).

use crate::{Model, Op, Sort};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Features {
    pub nodes: usize,
    pub inputs: usize,
    pub states: usize,
    pub state_bits: u64,
    pub array_states: usize,
    pub bad: usize,
    pub constraints: usize,
    pub fair: usize,
    pub justice: usize,
    pub max_width: u32,
    pub has_arrays: bool,
    /// Count of arithmetic operators (add, mul, div, rem, ...), a
    /// proxy for how much bit-blasting would cost.
    pub arith_ops: usize,
    pub op_counts: BTreeMap<&'static str, usize>,
}

impl Features {
    pub fn of(model: &Model) -> Features {
        let mut f = Features::default();
        for e in model.entries() {
            if let crate::Entry::Sort { sort, .. } = e {
                match sort {
                    Sort::BitVec(w) => f.max_width = f.max_width.max(*w),
                    Sort::Array { .. } => f.has_arrays = true,
                }
            }
        }
        for l in model.lines() {
            f.nodes += 1;
            *f.op_counts.entry(l.op.name()).or_default() += 1;
            match l.op {
                Op::Input => f.inputs += 1,
                Op::State => {
                    f.states += 1;
                    match l.sort.and_then(|s| model.sort(s)) {
                        Some(Sort::BitVec(w)) => f.state_bits += u64::from(w),
                        Some(Sort::Array { .. }) => f.array_states += 1,
                        None => {}
                    }
                }
                Op::Bad => f.bad += 1,
                Op::Constraint => f.constraints += 1,
                Op::Fair => f.fair += 1,
                Op::Justice => f.justice += 1,
                Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Sdiv
                | Op::Udiv
                | Op::Smod
                | Op::Srem
                | Op::Urem
                | Op::Neg
                | Op::Inc
                | Op::Dec => f.arith_ops += 1,
                _ => {}
            }
        }
        f
    }
}
