//! The signals a property depends on: the inputs and registers in the
//! cone of influence of its target in the Btor2 model, each with how many
//! register stages lie between it and the property.

use informal_ir::Op;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchematicPort {
    pub id: String,
    pub label: String,
    /// `"in"` or `"out"`.
    pub dir: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchematicNode {
    pub id: String,
    pub label: String,
    /// Full hierarchical signal name when this node corresponds to an RTL net/state/input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// `"input"`, `"register"`, `"mux"`, `"arith"`, `"cmp"`, `"logic"`, `"const"`, or `"target"`.
    pub kind: &'static str,
    pub op: String,
    pub width: u32,
    pub distance: u32,
    /// `true` if this node lies on the active `why_cex` backward causal slice.
    pub causal: bool,
    /// `true` if this node is a formal state-space hotspot (multiplier/divider or wide >=16b arithmetic).
    pub hotspot: bool,
    /// `true` if this is a register at the `max_depth` cut boundary whose fan-in can be expanded.
    pub expandable: bool,
    /// Power domain name owning this node when IEEE 1801 UPF/CPF is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_domain: Option<String>,
    pub ports: Vec<SchematicPort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchematicEdge {
    pub id: String,
    pub source: String,
    pub source_port: String,
    pub target: String,
    pub target_port: String,
    pub width: u32,
    pub inverted: bool,
    pub causal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchematicGraph {
    pub max_depth: u32,
    pub truncated: bool,
    pub nodes: Vec<SchematicNode>,
    pub edges: Vec<SchematicEdge>,
}

fn matches_causal_name(node_sig: &str, active: &[String]) -> bool {
    let node_leaf = node_sig.rsplit('.').next().unwrap_or(node_sig);
    active.iter().any(|a| {
        if a == node_sig || a == node_leaf {
            return true;
        }
        let a_leaf = a.rsplit('.').next().unwrap_or(a);
        a_leaf == node_leaf
            || node_sig.ends_with(&format!(".{a}"))
            || a.ends_with(&format!(".{node_sig}"))
    })
}

fn classify_op(op: Op, width: u32) -> (&'static str, &'static str, bool) {
    match op {
        Op::Input => ("input", "IN", false),
        Op::State => ("register", "DFF", width >= 32),
        Op::Bad => ("target", "ASSERT", false),
        Op::Justice => ("target", "FAIR", false),
        Op::Ite => ("mux", "MUX", false),
        Op::Mul | Op::Smulo | Op::Umulo => ("arith", "*", true),
        Op::Udiv | Op::Sdiv | Op::Urem | Op::Srem | Op::Smod => ("arith", "/", true),
        Op::Add | Op::Inc | Op::Uaddo | Op::Saddo => ("arith", "+", width >= 16),
        Op::Sub | Op::Dec | Op::Neg | Op::Usubo | Op::Ssubo => ("arith", "-", width >= 16),
        Op::Eq => ("cmp", "==", false),
        Op::Neq => ("cmp", "!=", false),
        Op::Ult | Op::Slt => ("cmp", "<", false),
        Op::Ulte | Op::Slte => ("cmp", "<=", false),
        Op::Ugt | Op::Sgt => ("cmp", ">", false),
        Op::Ugte | Op::Sgte => ("cmp", ">=", false),
        Op::And | Op::Redand => ("logic", "&", false),
        Op::Or | Op::Redor => ("logic", "|", false),
        Op::Xor | Op::Redxor | Op::Xnor => ("logic", "^", false),
        Op::Not => ("logic", "~", false),
        Op::Slice => ("logic", "[ : ]", false),
        Op::Concat => ("logic", "{ , }", false),
        Op::Uext | Op::Sext => ("logic", "EXT", false),
        Op::Sll | Op::Srl | Op::Sra | Op::Rol | Op::Ror => ("logic", ">>", width >= 16),
        Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones => {
            ("const", "CONST", false)
        }
        _ => ("logic", op.name(), false),
    }
}

/// Extract a bounded, port-aware RTL schematic subgraph from `btor` up to `max_depth`
/// sequential register stages and `max_nodes` nodes, highlighting `active_signals` (`why_cex`)
/// and arithmetic/state complexity hotspots.
pub fn schematic(
    btor: &str,
    max_depth: u32,
    max_nodes: usize,
    active_signals: &[String],
) -> Result<SchematicGraph, String> {
    let m = informal_ir::parse(btor).map_err(|e| e.to_string())?;
    let target = m
        .lines()
        .find(|l| matches!(l.op, Op::Bad | Op::Justice))
        .ok_or("the model has no target")?;
    let next_map: HashMap<u32, i64> = m
        .with_op(Op::Next)
        .filter_map(|l| Some((l.args.first()?.unsigned_abs() as u32, *l.args.get(1)?)))
        .collect();

    let cap = max_nodes.clamp(8, 250);
    let mut visited: HashMap<u32, u32> = HashMap::new();
    let mut nodes: Vec<SchematicNode> = Vec::new();
    let mut edges_raw: Vec<(u32, String, u32, String, bool)> = Vec::new();
    let mut truncated = false;

    // Target check sink node
    let target_nid = format!("n{}", target.id);
    let target_label = target
        .symbol
        .clone()
        .unwrap_or_else(|| target.op.name().to_uppercase());
    nodes.push(SchematicNode {
        id: target_nid.clone(),
        label: target_label,
        signal: target.symbol.clone(),
        kind: "target",
        op: target.op.name().to_string(),
        width: 1,
        distance: 0,
        causal: true,
        hotspot: false,
        expandable: false,
        power_domain: None,
        ports: vec![SchematicPort {
            id: format!("{target_nid}.chk"),
            label: "chk".into(),
            dir: "in",
        }],
    });
    visited.insert(target.id, 0);

    let mut queue: std::collections::VecDeque<(u32, u32)> = std::collections::VecDeque::new();
    if let Some(&arg0) = target.args.first() {
        let src_id = arg0.unsigned_abs() as u32;
        queue.push_back((src_id, 0));
        edges_raw.push((src_id, "y".into(), target.id, "chk".into(), arg0 < 0));
    }

    while let Some((id, dist)) = queue.pop_front() {
        if visited.contains_key(&id) {
            continue;
        }
        if nodes.len() >= cap {
            truncated = true;
            continue;
        }
        let Some(l) = m.line(id) else { continue };
        visited.insert(id, dist);

        let width = l.sort.and_then(|s| m.width(s)).unwrap_or(1);
        let (kind, sym_op, hotspot) = classify_op(l.op, width);
        let sig_opt = l.symbol.clone();
        let label = match (&sig_opt, l.op) {
            (Some(s), Op::Input | Op::State) => s.rsplit('.').next().unwrap_or(s).to_string(),
            (Some(s), _) => format!("{} ({sym_op})", s.rsplit('.').next().unwrap_or(s)),
            (None, Op::Slice) if l.nums.len() >= 2 => format!("[{}:{}]", l.nums[0], l.nums[1]),
            (None, Op::Const | Op::Constd | Op::Consth) if !l.nums.is_empty() => {
                let lit = &l.nums[0];
                if lit.len() > 8 {
                    format!("{width}'b…")
                } else {
                    lit.clone()
                }
            }
            (None, Op::Zero) => format!("{width}'d0"),
            (None, Op::One) => format!("{width}'d1"),
            _ => sym_op.to_string(),
        };

        let nid = format!("n{id}");
        let mut ports = Vec::new();
        let mut expandable = false;

        match l.op {
            Op::Input | Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones => {
                ports.push(SchematicPort {
                    id: format!("{nid}.y"),
                    label: "y".into(),
                    dir: "out",
                });
            }
            Op::State => {
                let has_next = next_map.contains_key(&id);
                if has_next && dist < max_depth {
                    ports.push(SchematicPort {
                        id: format!("{nid}.d"),
                        label: "D".into(),
                        dir: "in",
                    });
                    let nxt = next_map[&id];
                    let src_id = nxt.unsigned_abs() as u32;
                    queue.push_back((src_id, dist + 1));
                    edges_raw.push((src_id, "y".into(), id, "d".into(), nxt < 0));
                } else if has_next {
                    expandable = true;
                }
                ports.push(SchematicPort {
                    id: format!("{nid}.y"),
                    label: "Q".into(),
                    dir: "out",
                });
            }
            Op::Ite => {
                for (ix, pname, plabel) in [(0, "sel", "S"), (1, "t", "1"), (2, "f", "0")] {
                    ports.push(SchematicPort {
                        id: format!("{nid}.{pname}"),
                        label: plabel.into(),
                        dir: "in",
                    });
                    if let Some(&arg) = l.args.get(ix) {
                        let src_id = arg.unsigned_abs() as u32;
                        queue.push_back((src_id, dist));
                        edges_raw.push((src_id, "y".into(), id, pname.into(), arg < 0));
                    }
                }
                ports.push(SchematicPort {
                    id: format!("{nid}.y"),
                    label: "Y".into(),
                    dir: "out",
                });
            }
            _ => {
                for (ix, &arg) in l.args.iter().enumerate() {
                    let pname = format!("a{ix}");
                    let plabel = if l.args.len() == 1 || ix == 0 {
                        "a".to_string()
                    } else if ix == 1 {
                        "b".to_string()
                    } else {
                        pname.clone()
                    };
                    ports.push(SchematicPort {
                        id: format!("{nid}.{pname}"),
                        label: plabel,
                        dir: "in",
                    });
                    let src_id = arg.unsigned_abs() as u32;
                    queue.push_back((src_id, dist));
                    edges_raw.push((src_id, "y".into(), id, pname, arg < 0));
                }
                ports.push(SchematicPort {
                    id: format!("{nid}.y"),
                    label: "y".into(),
                    dir: "out",
                });
            }
        }

        let causal = sig_opt
            .as_deref()
            .is_some_and(|s| matches_causal_name(s, active_signals));
        nodes.push(SchematicNode {
            id: nid,
            label,
            signal: sig_opt,
            kind,
            op: l.op.name().to_string(),
            width,
            distance: dist,
            causal,
            hotspot,
            expandable,
            power_domain: None,
            ports,
        });
    }

    // Propagate causal flag along combinational operators between causal leaf signals and target
    let node_idx: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();
    if !active_signals.is_empty() {
        for _ in 0..4 {
            for &(src, _, dst, _, _) in &edges_raw {
                let (sn, dn) = (format!("n{src}"), format!("n{dst}"));
                if let (Some(&si), Some(&di)) = (node_idx.get(&sn), node_idx.get(&dn)) {
                    if nodes[si].causal && nodes[di].kind != "input" && nodes[di].signal.is_none() {
                        nodes[di].causal = true;
                    }
                }
            }
        }
    }

    let mut edges = Vec::new();
    for (idx, (src, sp, dst, dp, inv)) in edges_raw.into_iter().enumerate() {
        let (sn, dn) = (format!("n{src}"), format!("n{dst}"));
        let (Some(&si), Some(&di)) = (node_idx.get(&sn), node_idx.get(&dn)) else {
            continue;
        };
        let causal = nodes[si].causal && nodes[di].causal;
        edges.push(SchematicEdge {
            id: format!("e{idx}"),
            source: sn.clone(),
            source_port: format!("{sn}.{sp}"),
            target: dn.clone(),
            target_port: format!("{dn}.{dp}"),
            width: nodes[si].width,
            inverted: inv,
            causal,
            signal: nodes[si].signal.clone(),
        });
    }

    Ok(SchematicGraph {
        max_depth,
        truncated,
        nodes,
        edges,
    })
}

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

    #[test]
    fn schematic_extracts_ports_causal_paths_and_expandable_boundary() {
        let mux_model = "\
1 sort bitvec 1
2 sort bitvec 16
3 input 1 top.sel
4 input 2 top.a
5 input 2 top.b
6 mul 2 4 5
7 state 2 top.acc
8 ite 2 3 6 7
9 next 2 7 8
10 eq 1 7 4
11 bad 10 a_bad
";
        let g0 = schematic(mux_model, 0, 50, &["top.acc".to_string()]).unwrap();
        let reg0 = g0
            .nodes
            .iter()
            .find(|n| n.signal.as_deref() == Some("top.acc"))
            .unwrap();
        assert!(
            reg0.expandable,
            "at depth 0, register with next should be expandable"
        );
        assert!(reg0.causal);

        let g1 = schematic(
            mux_model,
            1,
            50,
            &["top.acc".to_string(), "top.sel".to_string()],
        )
        .unwrap();
        assert!(g1.nodes.iter().any(|n| n.kind == "mux"));
        let mul = g1.nodes.iter().find(|n| n.op == "mul").unwrap();
        assert!(mul.hotspot, "mul must be flagged as complexity hotspot");
        assert!(!g1.edges.is_empty());
    }
}
