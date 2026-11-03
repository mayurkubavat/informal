//! Unrolls a Btor2 transition system into SAT one time frame at a time.
//!
//! Frame 0 states take their `init` value, or are free when they have none
//! (or, for induction, always free). Frame k+1 states take the `next`
//! value computed in frame k. Inputs are fresh in every frame. Only the
//! cone of influence of the bad, constraint and next lines is encoded.

use crate::gates::{Gates, Word};
use anyhow::{anyhow, bail, Result};
use fdv_ir::{Line, Model, Op, Sort};
use std::collections::{BTreeSet, HashMap};

pub struct Unroller<'m> {
    m: &'m Model,
    /// Lines in the cone, in file order (arguments come first).
    cone: Vec<&'m Line>,
    /// state id -> next expression (signed node ref)
    next: HashMap<u32, i64>,
    /// state id -> init expression
    init: HashMap<u32, i64>,
    pub frames: Vec<HashMap<u32, Word>>,
    /// Constraint and bad literals per frame.
    pub constraints: Vec<Lit2>,
    pub bads: Vec<Lit2>,
    free_init: bool,
}

/// Literals of one frame.
pub type Lit2 = Vec<fdv_sat::Lit>;

impl<'m> Unroller<'m> {
    /// `free_init` leaves frame-0 states unconstrained (induction step).
    pub fn new(m: &'m Model, free_init: bool) -> Result<Self> {
        if m.with_op(Op::Bad).next().is_none() {
            bail!("model has no bad-state property");
        }
        if m.with_op(Op::Fair).next().is_some() || m.with_op(Op::Justice).next().is_some() {
            bail!("liveness (fair / justice) is not supported by this engine");
        }
        let mut next = HashMap::new();
        let mut init = HashMap::new();
        for l in m.with_op(Op::Next) {
            next.insert(l.args[0] as u32, l.args[1]);
        }
        for l in m.with_op(Op::Init) {
            init.insert(l.args[0] as u32, l.args[1]);
        }
        // Cone of influence.
        let mut need = BTreeSet::new();
        let mut stack: Vec<u32> = m
            .with_op(Op::Bad)
            .chain(m.with_op(Op::Constraint))
            .map(|l| l.args[0].unsigned_abs() as u32)
            .collect();
        while let Some(id) = stack.pop() {
            if !need.insert(id) {
                continue;
            }
            let l = m
                .line(id)
                .ok_or_else(|| anyhow!("dangling reference to {id}"))?;
            stack.extend(l.args.iter().map(|a| a.unsigned_abs() as u32));
            if l.op == Op::State {
                stack.extend(next.get(&id).map(|a| a.unsigned_abs() as u32));
                if !free_init {
                    stack.extend(init.get(&id).map(|a| a.unsigned_abs() as u32));
                }
            }
        }
        let cone: Vec<&Line> = m.lines().filter(|l| need.contains(&l.id)).collect();
        for l in &cone {
            if let Some(s) = l.sort {
                if matches!(m.sort(s), Some(Sort::Array { .. })) {
                    bail!("arrays are not supported by this engine yet");
                }
            }
        }
        Ok(Unroller {
            m,
            cone,
            next,
            init,
            frames: Vec::new(),
            constraints: Vec::new(),
            bads: Vec::new(),
            free_init,
        })
    }

    /// All state bits of frame `k`, for simple-path constraints.
    pub fn state_bits(&self, k: usize) -> Word {
        self.cone
            .iter()
            .filter(|l| l.op == Op::State)
            .flat_map(|l| self.frames[k][&l.id].iter().copied())
            .collect()
    }

    fn width(&self, l: &Line) -> Result<u32> {
        let s = l.sort.ok_or_else(|| anyhow!("line {} has no sort", l.id))?;
        self.m
            .width(s)
            .ok_or_else(|| anyhow!("line {} is not a bit-vector", l.id))
    }

    fn get(&self, frame: &HashMap<u32, Word>, g: &Gates, a: i64) -> Result<Word> {
        let w = frame
            .get(&(a.unsigned_abs() as u32))
            .ok_or_else(|| anyhow!("node {} used before it is defined", a.abs()))?;
        Ok(if a < 0 { g.not_w(w) } else { w.clone() })
    }

    /// Encode the next frame; returns its index.
    pub fn add_frame(&mut self, g: &mut Gates) -> Result<usize> {
        let k = self.frames.len();
        let mut cur: HashMap<u32, Word> = HashMap::new();
        let mut late_init = Vec::new();
        for l in self.cone.clone() {
            let w = match l.op {
                Op::State => {
                    if k == 0 {
                        match self.init.get(&l.id) {
                            Some(&e) if !self.free_init => match self.get(&cur, g, e) {
                                Ok(v) => v,
                                // The init value comes later in the file: tie it
                                // to a fresh word once the frame is encoded.
                                Err(_) => {
                                    let v = g.fresh_word(self.width(l)?);
                                    late_init.push((v.clone(), e));
                                    v
                                }
                            },
                            _ => g.fresh_word(self.width(l)?),
                        }
                    } else {
                        let e = self.next.get(&l.id).copied();
                        match e {
                            Some(e) => self.get(&self.frames[k - 1], g, e)?,
                            None => g.fresh_word(self.width(l)?),
                        }
                    }
                }
                Op::Input => g.fresh_word(self.width(l)?),
                Op::Bad | Op::Constraint | Op::Output | Op::Init | Op::Next => continue,
                _ => self.encode(g, &cur, l)?,
            };
            cur.insert(l.id, w);
        }
        for (v, e) in late_init {
            let x = self.get(&cur, g, e)?;
            let eq = g.eq_w(&v, &x);
            g.sat.add_clause(&[eq]);
        }
        let mut cons = Vec::new();
        for l in self.m.with_op(Op::Constraint) {
            cons.push(self.get(&cur, g, l.args[0])?[0]);
        }
        let mut bads = Vec::new();
        for l in self.m.with_op(Op::Bad) {
            bads.push(self.get(&cur, g, l.args[0])?[0]);
        }
        for &c in &cons {
            g.sat.add_clause(&[c]);
        }
        self.constraints.push(cons);
        self.bads.push(bads);
        self.frames.push(cur);
        Ok(k)
    }

    fn encode(&self, g: &mut Gates, f: &HashMap<u32, Word>, l: &Line) -> Result<Word> {
        let w = self.width(l)? as usize;
        let arg = |i: usize| -> Result<Word> { self.get(f, g, l.args[i]) };
        let num = |i: usize| -> Result<usize> {
            l.nums
                .get(i)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("line {}: bad number", l.id))
        };
        let a = if !l.args.is_empty() {
            Some(arg(0)?)
        } else {
            None
        };
        let b = if l.args.len() > 1 {
            Some(arg(1)?)
        } else {
            None
        };
        let c = if l.args.len() > 2 {
            Some(arg(2)?)
        } else {
            None
        };
        let (a_, b_) = (a.as_ref(), b.as_ref());
        let a1 = || a_.unwrap();
        let b1 = || b_.unwrap();
        let bit = |x: Lit1| vec![x];
        type Lit1 = fdv_sat::Lit;
        Ok(match l.op {
            Op::Zero => g.konst((0..w).map(|_| false)),
            Op::One => g.konst((0..w).map(|i| i == 0)),
            Op::Ones => g.konst((0..w).map(|_| true)),
            Op::Const => {
                let s = &l.nums[0];
                g.konst(
                    (0..w).map(|i| s.as_bytes().get(s.len().wrapping_sub(1 + i)) == Some(&b'1')),
                )
            }
            Op::Constd => g.konst(decimal_bits(&l.nums[0], w)?),
            Op::Consth => g.konst(hex_bits(&l.nums[0], w)?),
            Op::Sext | Op::Uext => {
                let mut v = a1().clone();
                let fill = if l.op == Op::Sext {
                    *v.last().unwrap()
                } else {
                    g.f()
                };
                v.resize(w, fill);
                v
            }
            Op::Slice => {
                let (hi, lo) = (num(0)?, num(1)?);
                a1()[lo..=hi].to_vec()
            }
            Op::Not => g.not_w(a1()),
            Op::Inc => {
                let one = g.konst((0..w).map(|i| i == 0));
                g.add(a1(), &one)
            }
            Op::Dec => {
                let one = g.konst((0..w).map(|i| i == 0));
                g.sub_c(a1(), &one).0
            }
            Op::Neg => g.neg(a1()),
            Op::Redand => bit(g.and_all(a1())),
            Op::Redor => bit(g.or_all(a1())),
            Op::Redxor => bit(a1().iter().fold(g.f(), |acc, &x| g.xor(acc, x))),
            Op::Iff | Op::Xnor => g.zip(a1(), b1(), |g, x, y| -g.xor(x, y)),
            Op::Implies => g.zip(a1(), b1(), |g, x, y| g.or(-x, y)),
            Op::Eq => bit(g.eq_w(a1(), b1())),
            Op::Neq => bit(-g.eq_w(a1(), b1())),
            Op::Ult => bit(g.ult(a1(), b1())),
            Op::Ulte => bit(-g.ult(b1(), a1())),
            Op::Ugt => bit(g.ult(b1(), a1())),
            Op::Ugte => bit(-g.ult(a1(), b1())),
            Op::Slt => bit(g.slt(a1(), b1())),
            Op::Slte => bit(-g.slt(b1(), a1())),
            Op::Sgt => bit(g.slt(b1(), a1())),
            Op::Sgte => bit(-g.slt(a1(), b1())),
            Op::And => g.zip(a1(), b1(), Gates::and),
            Op::Nand => g.zip(a1(), b1(), |g, x, y| -g.and(x, y)),
            Op::Or => g.zip(a1(), b1(), Gates::or),
            Op::Nor => g.zip(a1(), b1(), |g, x, y| -g.or(x, y)),
            Op::Xor => g.zip(a1(), b1(), Gates::xor),
            Op::Add => g.add(a1(), b1()),
            Op::Sub => g.sub_c(a1(), b1()).0,
            Op::Mul => g.mul(a1(), b1()),
            Op::Udiv => g.udivrem(a1(), b1()).0,
            Op::Urem => g.udivrem(a1(), b1()).1,
            Op::Sdiv => g.sdiv(a1(), b1()),
            Op::Srem => g.srem(a1(), b1()),
            Op::Smod => g.smod(a1(), b1()),
            Op::Sll => {
                let f0 = g.f();
                g.shift(a1(), b1(), true, f0)
            }
            Op::Srl => {
                let f0 = g.f();
                g.shift(a1(), b1(), false, f0)
            }
            Op::Sra => {
                let s = *a1().last().unwrap();
                g.shift(a1(), b1(), false, s)
            }
            Op::Rol => g.rotate(a1(), b1(), true),
            Op::Ror => g.rotate(a1(), b1(), false),
            Op::Concat => {
                // `concat a b`: a is the high part.
                let mut v = b1().clone();
                v.extend_from_slice(a1());
                v
            }
            Op::Ite => {
                let c = c.as_ref().unwrap();
                g.ite_w(a1()[0], b1(), c)
            }
            Op::Uaddo => {
                let f0 = g.f();
                bit(g.add_c(a1(), b1(), f0).1)
            }
            Op::Saddo => {
                let s = g.add(a1(), b1());
                let (sa, sb, ss) = (
                    *a1().last().unwrap(),
                    *b1().last().unwrap(),
                    *s.last().unwrap(),
                );
                let same = -g.xor(sa, sb);
                let flip = g.xor(ss, sa);
                bit(g.and(same, flip))
            }
            Op::Usubo => bit(g.ult(a1(), b1())),
            Op::Ssubo => {
                let s = g.sub_c(a1(), b1()).0;
                let (sa, sb, ss) = (
                    *a1().last().unwrap(),
                    *b1().last().unwrap(),
                    *s.last().unwrap(),
                );
                let differ = g.xor(sa, sb);
                let flip = g.xor(ss, sa);
                bit(g.and(differ, flip))
            }
            Op::Umulo => {
                let n = a1().len();
                let f0 = g.f();
                let mut x = a1().clone();
                let mut y = b1().clone();
                x.resize(2 * n, f0);
                y.resize(2 * n, f0);
                let p = g.mul(&x, &y);
                bit(g.or_all(&p[n..]))
            }
            Op::Smulo => {
                let n = a1().len();
                let (sa, sb) = (*a1().last().unwrap(), *b1().last().unwrap());
                let mut x = a1().clone();
                let mut y = b1().clone();
                x.resize(2 * n, sa);
                y.resize(2 * n, sb);
                let p = g.mul(&x, &y);
                // Overflow unless bits n-1 .. 2n-1 all equal.
                let top = p[n - 1];
                let diffs: Vec<_> = p[n..].iter().map(|&q| g.xor(q, top)).collect();
                bit(g.or_all(&diffs))
            }
            Op::Udivo => bit(g.f()),
            Op::Sdivo => {
                let n = a1().len();
                let min = g.konst((0..n).map(|i| i == n - 1));
                let ones = g.konst((0..n).map(|_| true));
                let x = g.eq_w(a1(), &min);
                let y = g.eq_w(b1(), &ones);
                bit(g.and(x, y))
            }
            Op::Read | Op::Write => bail!("arrays are not supported by this engine yet"),
            op => bail!("unexpected operator {} in expression", op.name()),
        })
    }
}

/// Bits (LSB first) of a decimal literal, two's complement for negatives.
fn decimal_bits(s: &str, w: usize) -> Result<Vec<bool>> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s),
    };
    let mut mag = vec![false; w];
    for ch in digits.chars() {
        // mag = mag * 10 + d (mod 2^w), bit by bit with a carry
        let mut carry = ch
            .to_digit(10)
            .ok_or_else(|| anyhow!("bad decimal constant {s}"))? as u64;
        for b in mag.iter_mut() {
            let v = *b as u64 * 10 + carry;
            *b = v & 1 == 1;
            carry = v >> 1;
        }
    }
    if neg {
        let mut carry = true;
        for b in mag.iter_mut() {
            let v = !*b;
            *b = v ^ carry;
            carry = v && carry;
        }
    }
    Ok(mag)
}

fn hex_bits(s: &str, w: usize) -> Result<Vec<bool>> {
    let mut bits = Vec::with_capacity(s.len() * 4);
    for ch in s.chars().rev() {
        let d = ch
            .to_digit(16)
            .ok_or_else(|| anyhow!("bad hex constant {s}"))?;
        for i in 0..4 {
            bits.push((d >> i) & 1 == 1);
        }
    }
    bits.resize(w, false);
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(bits: &[bool]) -> u64 {
        bits.iter().enumerate().map(|(i, &b)| (b as u64) << i).sum()
    }

    #[test]
    fn constants() {
        assert_eq!(val(&decimal_bits("1234", 16).unwrap()), 1234);
        assert_eq!(val(&decimal_bits("-1", 8).unwrap()), 255);
        assert_eq!(val(&decimal_bits("300", 8).unwrap()), 300 % 256);
        assert_eq!(val(&hex_bits("ff", 12).unwrap()), 255);
    }
}
