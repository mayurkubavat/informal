//! Counterexample and cover traces as VCD (docs/ui/trace-format.md).
//!
//! An engine's named witness (`informalw 1`: free initial latch values and
//! per-frame inputs by signal name) is replayed on the property's own,
//! unsimplified model, so every input and register gets a value even when
//! the engine solved a simplified copy. The bad (or cover) output must be
//! true at the reported depth, or no trace is written: a trace we cannot
//! replay is never shown. Bits group back into words by their `name[i]`
//! suffix and dotted names into scopes; FPV helpers (`informal_*`) go under an
//! `informal` scope with the property's own lane. Named `output` lines (the
//! slang lowering's combinational nets, in `prop_N.trace.btor2`) are
//! replayed and written too.

use anyhow::{bail, Context, Result};
use informal_aig::{is_neg, var, Aig, Init, Lit, Node};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;

/// A parsed `informalw 1` witness.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Witness {
    pub depth: usize,
    pub init: HashMap<String, bool>,
    pub frames: Vec<HashMap<String, bool>>,
}

/// Parse a named witness. `None` when the text is not one (an external
/// engine's raw output, for example).
pub fn parse_witness(text: &str) -> Option<Witness> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next()? != "informalw 1" {
        return None;
    }
    let mut w = Witness::default();
    let pairs = |rest: &str| -> Option<HashMap<String, bool>> {
        rest.split_whitespace()
            .map(|kv| {
                let (k, v) = kv.rsplit_once('=')?;
                Some((k.to_string(), v == "1"))
            })
            .collect()
    };
    for l in lines {
        let (head, rest) = l.split_once(' ').unwrap_or((l, ""));
        match head {
            "depth" => w.depth = rest.trim().parse().ok()?,
            "init" => w.init = pairs(rest)?,
            h if h.starts_with('@') => {
                let f: usize = h[1..].parse().ok()?;
                if f != w.frames.len() {
                    return None;
                }
                w.frames.push(pairs(rest)?);
            }
            _ => return None,
        }
    }
    (w.frames.len() == w.depth + 1).then_some(w)
}

/// One VCD variable: its scope path, leaf name and per-frame value bits
/// (LSB first).
struct Var {
    scope: Vec<String>,
    name: String,
    bits: Vec<Vec<Option<bool>>>,
}

/// Replay `w` on the model at `model` (Btor2 or AIGER) and write the trace
/// as VCD text. `prop` names the property lane, `clock` the clock signal.
pub fn vcd_from_witness(model: &Path, w: &Witness, prop: &str, clock: &str) -> Result<String> {
    let g = informal_aig::load(model).with_context(|| format!("loading {}", model.display()))?;
    let frames = simulate(&g, w)?;
    let bad = |f: usize| g.bads.iter().any(|&b| lit(&frames[f], b));
    if !bad(w.depth) {
        bail!(
            "the witness does not reach the property at depth {}",
            w.depth
        );
    }

    // Named inputs and latches, grouped into words by `name[i]`.
    let mut words: BTreeMap<String, Vec<(usize, u32)>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let named = g
        .inputs
        .iter()
        .map(|&i| var(i))
        .chain(g.latches.iter().map(|l| var(l.lit)));
    for v in named {
        let Some(n) = g.names.get(&v) else { continue };
        let (base, idx) = split_index(n);
        if !words.contains_key(base) {
            order.push(base.to_string());
        }
        words.entry(base.to_string()).or_default().push((idx, v));
    }
    let top = order
        .iter()
        .find_map(|n| n.split_once('.').map(|(t, _)| t.to_string()))
        .unwrap_or_else(|| "top".into());
    let mut vars = vec![Var {
        scope: vec![top.clone()],
        name: clock.rsplit('.').next().unwrap_or(clock).to_string(),
        bits: Vec::new(), // the clock is written separately
    }];
    for base in &order {
        let bits_of = &words[base];
        let width = bits_of.iter().map(|&(i, _)| i + 1).max().unwrap_or(1);
        let mut parts: Vec<String> = base.split('.').map(str::to_string).collect();
        let leaf = parts.pop().unwrap_or_default();
        if leaf.starts_with("informal_") {
            parts = vec!["informal".into()];
        } else if parts.is_empty() {
            parts.push(top.clone());
        }
        let bits = (0..frames.len())
            .map(|f| {
                let mut b = vec![None; width];
                for &(i, v) in bits_of {
                    b[i] = Some(frames[f][v as usize]);
                }
                b
            })
            .collect();
        vars.push(Var {
            scope: parts,
            name: leaf,
            bits,
        });
    }
    // Combinational nets the model names with `output` lines (the slang
    // lowering's trace section), unless an input or register already has
    // the name. Their values come from the replay like everything else.
    for (n, bits_of) in &g.outputs {
        if words.contains_key(n.as_str()) || bits_of.is_empty() {
            continue;
        }
        let mut parts: Vec<String> = n.split('.').map(str::to_string).collect();
        let leaf = parts.pop().unwrap_or_default();
        if leaf.starts_with("informal_") {
            continue;
        }
        if parts.is_empty() {
            parts.push(top.clone());
        }
        let bits = (0..frames.len())
            .map(|f| bits_of.iter().map(|&b| Some(lit(&frames[f], b))).collect())
            .collect();
        vars.push(Var {
            scope: parts,
            name: leaf,
            bits,
        });
    }
    vars.push(Var {
        scope: vec!["informal".into()],
        name: prop.to_string(),
        bits: (0..frames.len()).map(|f| vec![Some(bad(f))]).collect(),
    });
    Ok(write_vcd(&vars, frames.len(), prop))
}

/// `q[3]` -> ("q", 3); `en` -> ("en", 0).
fn split_index(n: &str) -> (&str, usize) {
    if let Some(open) = n.strip_suffix(']').and_then(|s| s.rfind('[')) {
        if let Ok(i) = n[open + 1..n.len() - 1].parse() {
            return (&n[..open], i);
        }
    }
    (n, 0)
}

fn lit(val: &[bool], l: Lit) -> bool {
    val[var(l) as usize] ^ is_neg(l)
}

/// Node values per frame, 0..=depth. Inputs and free initial latches the
/// witness does not name are 0 (they are don't-cares for the property:
/// the engine's simplified model did not need them). Constraints must hold.
fn simulate(g: &Aig, w: &Witness) -> Result<Vec<Vec<bool>>> {
    let named = |m: &HashMap<String, bool>, v: u32| {
        g.names
            .get(&v)
            .and_then(|n| m.get(n))
            .copied()
            .unwrap_or(false)
    };
    let mut state: Vec<bool> = g
        .latches
        .iter()
        .map(|l| match l.init {
            Init::Zero => false,
            Init::One => true,
            _ => named(&w.init, var(l.lit)),
        })
        .collect();
    let mut out = Vec::with_capacity(w.depth + 1);
    for f in 0..=w.depth {
        let mut val = vec![false; g.nodes.len()];
        let mut li = 0;
        for v in 0..g.nodes.len() {
            val[v] = match g.nodes[v] {
                Node::Const => false,
                Node::Input => named(&w.frames[f], v as u32),
                Node::Latch => {
                    li += 1;
                    state[li - 1]
                }
                Node::And(a, b) => lit(&val, a) && lit(&val, b),
            };
        }
        if f == 0 {
            // Initial values given by an expression of other signals.
            let mut fixed = false;
            for (i, l) in g.latches.iter().enumerate() {
                if let Init::Expr(e) = l.init {
                    let want = lit(&val, e);
                    if state[i] != want {
                        state[i] = want;
                        fixed = true;
                    }
                }
            }
            if fixed {
                // Recompute frame 0 with the corrected initial state.
                let mut li = 0;
                for v in 0..g.nodes.len() {
                    if let Node::Latch = g.nodes[v] {
                        li += 1;
                        val[v] = state[li - 1];
                    } else if let Node::And(a, b) = g.nodes[v] {
                        val[v] = lit(&val, a) && lit(&val, b);
                    }
                }
            }
        }
        if let Some(c) = g.constraints.iter().position(|&c| !lit(&val, c)) {
            bail!("the witness breaks constraint {c} at frame {f}");
        }
        for (i, l) in g.latches.iter().enumerate() {
            state[i] = lit(&val, l.next);
        }
        out.push(val);
    }
    Ok(out)
}

/// VCD identifier for variable `i`: printable ASCII from `!`, base 94.
fn ident(mut i: usize) -> String {
    let mut s = String::new();
    loop {
        s.push((b'!' + (i % 94) as u8) as char);
        i /= 94;
        if i == 0 {
            return s;
        }
        i -= 1;
    }
}

fn bits_text(b: &[Option<bool>]) -> String {
    b.iter()
        .rev()
        .map(|x| match x {
            Some(true) => '1',
            Some(false) => '0',
            None => 'x',
        })
        .collect()
}

/// One cycle is 10 time units: values change at 10f with the clock rising,
/// the clock falls at 10f + 5. `vars[0]` is the clock.
fn write_vcd(vars: &[Var], frames: usize, prop: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "$comment informal trace of {prop}: {frames} cycles $end");
    s += "$timescale 1ns $end\n";
    // Scopes in first-seen order; variables grouped under them.
    let mut scopes: Vec<&[String]> = Vec::new();
    for v in vars {
        if !scopes.contains(&v.scope.as_slice()) {
            scopes.push(&v.scope);
        }
    }
    for sc in &scopes {
        for part in sc.iter() {
            let _ = writeln!(s, "$scope module {part} $end");
        }
        for (i, v) in vars
            .iter()
            .enumerate()
            .filter(|(_, v)| v.scope.as_slice() == *sc)
        {
            let width = if i == 0 {
                1
            } else {
                v.bits.first().map_or(1, Vec::len)
            };
            let kind = if i == 0 { "wire" } else { "reg" };
            let range = if width > 1 {
                format!(" [{}:0]", width - 1)
            } else {
                String::new()
            };
            let _ = writeln!(s, "$var {kind} {width} {} {}{range} $end", ident(i), v.name);
        }
        for _ in sc.iter() {
            s += "$upscope $end\n";
        }
    }
    s += "$enddefinitions $end\n";
    let mut last: Vec<Option<String>> = vec![None; vars.len()];
    for f in 0..frames {
        let _ = writeln!(s, "#{}", 10 * f);
        if f == 0 {
            s += "$dumpvars\n";
        }
        let _ = writeln!(s, "1{}", ident(0));
        for (i, v) in vars.iter().enumerate().skip(1) {
            let b = &v.bits[f];
            let text = bits_text(b);
            if last[i].as_deref() == Some(text.as_str()) {
                continue;
            }
            if b.len() == 1 {
                let _ = writeln!(s, "{text}{}", ident(i));
            } else {
                let _ = writeln!(s, "b{text} {}", ident(i));
            }
            last[i] = Some(text);
        }
        if f == 0 {
            s += "$end\n";
        }
        let _ = writeln!(s, "#{}\n0{}", 10 * f + 5, ident(0));
    }
    let _ = writeln!(s, "#{}", 10 * frames);
    s
}

/// A file name for a property's trace: `<prop>.vcd`, with characters that
/// do not belong in a file name replaced.
pub fn file_name(prop: &str) -> String {
    let safe: String = prop
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.vcd", safe.trim_start_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 2-bit counter with an enable input; bad when it reaches 3.
    const MODEL: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 top.en
4 state 2 top.q
5 zero 2
6 init 2 4 5
7 one 2
8 add 2 4 7
9 ite 2 3 8 4
10 next 2 4 9
11 constd 2 3
12 eq 1 4 11
13 bad 12
";

    fn model() -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("informal-trace-test-{}.btor2", std::process::id()));
        std::fs::write(&p, MODEL).unwrap();
        p
    }

    #[test]
    fn parses_witness() {
        let w =
            parse_witness("informalw 1\ndepth 1\ninit a=1\n@0 top.en=1 top.q[0]=0\n@1\n").unwrap();
        assert_eq!(w.depth, 1);
        assert!(w.init["a"]);
        assert!(!w.frames[0]["top.q[0]"]);
        assert!(w.frames[1].is_empty());
        assert!(parse_witness("sat\nb0\n").is_none());
        assert!(parse_witness("informalw 1\ndepth 2\ninit\n@0\n").is_none());
    }

    #[test]
    fn replays_and_writes_vcd() {
        let p = model();
        // en low in frame 1: q = 0,1,1,2,3
        let mut w = Witness {
            depth: 4,
            ..Default::default()
        };
        for f in 0..5 {
            w.frames.push([("top.en".to_string(), f != 1)].into());
        }
        let vcd = vcd_from_witness(&p, &w, "a_never3", "clk").unwrap();
        assert!(vcd.contains("$scope module top $end"), "{vcd}");
        assert!(vcd.contains("$var wire 1 ! clk $end"));
        assert!(vcd.contains("$var reg 2 # q [1:0] $end"), "{vcd}");
        assert!(
            vcd.contains("$scope module informal $end\n$var reg 1 $ a_never3 $end"),
            "{vcd}"
        );
        // q at frame 4 (#40) is 3; the property lane rises there.
        let at40 = vcd
            .split("#40\n")
            .nth(1)
            .unwrap()
            .split("#45")
            .next()
            .unwrap();
        assert!(at40.contains("b11 #") && at40.contains("1$"), "{at40}");
        assert!(vcd.trim_end().ends_with("#50"));

        // Stopping one frame short does not reach the bad state.
        w.depth = 3;
        w.frames.pop();
        assert!(vcd_from_witness(&p, &w, "a_never3", "clk").is_err());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn names() {
        assert_eq!(split_index("counter.q[12]"), ("counter.q", 12));
        assert_eq!(split_index("en"), ("en", 0));
        assert_eq!(split_index("a[b]"), ("a[b]", 0));
        assert_eq!(file_name("a_never9"), "a_never9.vcd");
        assert_eq!(file_name("../x/y z"), "_x_y_z.vcd");
        assert_eq!(ident(0), "!");
        assert_eq!(ident(93), "~");
        assert_eq!(ident(94), "!!");
    }
}
