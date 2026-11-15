//! The signals a property depends on: the inputs and registers in the
//! cone of influence of its target in the Btor2 model, each with how many
//! register stages lie between it and the property.

use informal_ir::Op;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConeSignal {
    pub name: String,
    /// Bits; 0 for a memory.
    pub width: u32,
    /// `input`, `register` or `memory`.
    pub kind: &'static str,
    /// Register stages from the property: 0 for what its expression reads
    /// in the same cycle.
    pub distance: u32,
    /// State the tool added to check the property (a `$past` copy, a step
    /// counter), not a signal of the design.
    pub helper: bool,
}

/// The cone of the model's first target (`bad` or `justice` line), nearest
/// first. Signals without a name in the model are left out.
pub fn cone(btor: &str) -> Result<Vec<ConeSignal>, String> {
    let m = informal_ir::parse(btor).map_err(|e| e.to_string())?;
    let target = m
        .lines()
        .find(|l| matches!(l.op, Op::Bad | Op::Justice))
        .ok_or("the model has no target")?;
    let next: HashMap<u32, u32> = m
        .with_op(Op::Next)
        .filter_map(|l| {
            Some((
                l.args.first()?.unsigned_abs() as u32,
                l.args.get(1)?.unsigned_abs() as u32,
            ))
        })
        .collect();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    let mut level: Vec<u32> = target
        .args
        .iter()
        .map(|a| a.unsigned_abs() as u32)
        .collect();
    let mut distance = 0;
    while !level.is_empty() {
        let mut registers = Vec::new();
        let mut stack = std::mem::take(&mut level);
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(l) = m.line(id) else { continue };
            if !matches!(l.op, Op::Input | Op::State) {
                stack.extend(l.args.iter().map(|a| a.unsigned_abs() as u32));
                continue;
            }
            if l.op == Op::State {
                registers.push(id);
            }
            let Some(name) = &l.symbol else { continue };
            let width = l.sort.and_then(|s| m.width(s));
            let leaf = name.rsplit('.').next().unwrap_or(name);
            out.push(ConeSignal {
                name: name.clone(),
                width: width.unwrap_or(0),
                kind: match (l.op, width) {
                    (Op::Input, _) => "input",
                    (_, None) => "memory",
                    _ => "register",
                },
                distance,
                helper: leaf.starts_with("informal_") || leaf.starts_with("_sva_"),
            });
        }
        level = registers
            .iter()
            .filter_map(|r| next.get(r).copied())
            .collect();
        distance += 1;
    }
    out.sort_by(|a, b| (a.distance, &a.name).cmp(&(b.distance, &b.name)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // q follows d by one cycle; bad when q and en are both 1. `other` is
    // not in the cone.
    const MODEL: &str = "\
1 sort bitvec 1
2 input 1 top.en
3 input 1 top.d
4 input 1 top.other
5 state 1 top.q
6 next 1 5 3
7 and 1 5 2
8 bad 7 a
";

    #[test]
    fn inputs_and_registers_by_distance() {
        let c = cone(MODEL).unwrap();
        let got: Vec<(&str, &str, u32)> = c
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.distance))
            .collect();
        assert_eq!(
            got,
            [
                ("top.en", "input", 0),
                ("top.q", "register", 0),
                ("top.d", "input", 1)
            ]
        );
    }
}
