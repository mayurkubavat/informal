//! External engines' counterexamples as informal named witnesses.
//!
//! informal's trace path (informal-fpv `trace.rs`, docs/ui/trace-format.md) replays
//! a *named* witness, `informalw 1`: free initial latch values and per-frame
//! inputs keyed by bit name (`counter.q[2]`, `counter.en`). External model
//! checkers print their counterexamples positionally instead:
//!
//! * the **AIGER witness** format (AIGER 1.9; rIC3 on `.aig`, ABC's
//!   `write_cex -a -t`): a status line `1`, the property line `b<k>`, the
//!   initial latch values (one character per latch, `x` for don't care),
//!   then one line of input values per frame, ending with `.`;
//! * the **Btor2 witness** format (rIC3 on `.btor2`, BtorMC, Pono): `sat`,
//!   `b<k>`, then per frame `#<k>` with state assignments and `@<k>` with
//!   input assignments, each `<index> <bits MSB first> [symbol]`, where the
//!   index counts `state` / `input` lines in declaration order; ends with `.`.
//!
//! The converters here map positions to names: AIGER from the model's
//! input and latch names, Btor2 from its `state` / `input` lines (bit-blasted
//! names as `informal_aig::btor` gives them: `name` for one bit, else `name[i]`).
//! They only translate; the caller replays the result on the model, so a
//! witness for some other (preprocessed) model is caught there.

use std::collections::BTreeMap;

fn bit(c: char) -> Option<Option<bool>> {
    match c {
        '0' => Some(Some(false)),
        '1' => Some(Some(true)),
        'x' | 'X' => Some(None),
        _ => None,
    }
}

/// The witness section of an engine's output, in the given format: from
/// the status line (`1` followed by `b<k>` for AIGER; `sat` for Btor2)
/// through the closing `.` (or the end). `None` when there is none.
pub fn extract(output: &str, btor2: bool) -> Option<String> {
    let lines: Vec<&str> = output.lines().map(str::trim_end).collect();
    let start = (0..lines.len()).find(|&i| {
        let next = lines.get(i + 1).map(|l| l.trim());
        let is_b = next.is_some_and(|l| {
            l.starts_with('b') && l[1..].chars().all(|c| c.is_ascii_digit()) && l.len() > 1
        });
        if btor2 {
            lines[i].trim() == "sat" && is_b
        } else {
            lines[i].trim() == "1" && is_b
        }
    })?;
    let mut out = String::new();
    for l in &lines[start..] {
        out += l.trim();
        out.push('\n');
        if l.trim() == "." {
            break;
        }
    }
    Some(out)
}

/// One named bit assignment list, in a stable order.
fn pairs(m: &BTreeMap<String, bool>) -> String {
    m.iter()
        .map(|(k, v)| format!(" {k}={}", u8::from(*v)))
        .collect()
}

fn render(init: &BTreeMap<String, bool>, frames: &[BTreeMap<String, bool>]) -> String {
    let mut s = format!(
        "informalw 1\ndepth {}\ninit{}",
        frames.len() - 1,
        pairs(init)
    );
    for (f, m) in frames.iter().enumerate() {
        s += &format!("\n@{f}{}", pairs(m));
    }
    s.push('\n');
    s
}

/// An AIGER witness as a named witness. `inputs` and `latches` are the
/// model's input and latch names in AIGER order (`None`: unnamed, left
/// out). Don't-care values are left out of `init` (the latch's own reset
/// applies) and read as 0 for inputs.
pub fn from_aiger(
    witness: &str,
    inputs: &[Option<String>],
    latches: &[Option<String>],
) -> Result<String, String> {
    let mut lines = witness
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('c'));
    if lines.next() != Some("1") {
        return Err("not an AIGER witness (no `1` status line)".into());
    }
    match lines.next() {
        Some(b) if b.starts_with('b') || b.starts_with('j') => {}
        _ => return Err("AIGER witness without a property line".into()),
    }
    let init_line = lines.next().ok_or("AIGER witness without initial state")?;
    if init_line.chars().count() != latches.len() {
        return Err(format!(
            "AIGER witness has {} initial latch values, the model {} latches",
            init_line.chars().count(),
            latches.len()
        ));
    }
    let mut init = BTreeMap::new();
    for (c, name) in init_line.chars().zip(latches) {
        let v = bit(c).ok_or_else(|| format!("bad latch value `{c}`"))?;
        if let (Some(v), Some(n)) = (v, name) {
            init.insert(n.clone(), v);
        }
    }
    let mut frames = Vec::new();
    for l in lines {
        if l == "." {
            break;
        }
        if l.chars().count() != inputs.len() {
            return Err(format!(
                "AIGER witness frame {} has {} input values, the model {} inputs",
                frames.len(),
                l.chars().count(),
                inputs.len()
            ));
        }
        let mut m = BTreeMap::new();
        for (c, name) in l.chars().zip(inputs) {
            let v = bit(c).ok_or_else(|| format!("bad input value `{c}`"))?;
            if let Some(n) = name {
                m.insert(n.clone(), v.unwrap_or(false));
            }
        }
        frames.push(m);
    }
    if frames.is_empty() {
        return Err("AIGER witness without frames".into());
    }
    Ok(render(&init, &frames))
}

/// Declared signals in declaration order, as (name, width).
pub type Decls = Vec<(String, u32)>;

/// A Btor2 model's `input` and `state` lines in declaration order, as
/// (name, width): the symbol, or `i<id>` / `s<id>` like `informal_aig::btor`.
pub fn btor2_symbols(model: &str) -> (Decls, Decls) {
    let mut sorts: BTreeMap<&str, u32> = BTreeMap::new();
    let (mut inputs, mut states) = (Vec::new(), Vec::new());
    for l in model.lines() {
        let l = l.split(';').next().unwrap_or("");
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            [id, "sort", "bitvec", width, ..] => {
                sorts.insert(id, width.parse().unwrap_or(0));
            }
            [id, kind @ ("input" | "state"), sort, rest @ ..] => {
                let width = sorts.get(sort).copied().unwrap_or(0);
                let prefix = if *kind == "input" { "i" } else { "s" };
                let name = rest
                    .first()
                    .map_or_else(|| format!("{prefix}{id}"), |s| s.to_string());
                if *kind == "input" {
                    inputs.push((name, width));
                } else {
                    states.push((name, width));
                }
            }
            _ => {}
        }
    }
    (inputs, states)
}

/// A Btor2 witness as a named witness, for the Btor2 model text `model`.
/// Frame 0's state assignments (`#0`) give the initial values; later state
/// frames, which only restate what the model computes, are ignored.
pub fn from_btor2(witness: &str, model: &str) -> Result<String, String> {
    let (inputs, states) = btor2_symbols(model);
    let mut lines = witness
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';'));
    if lines.next() != Some("sat") {
        return Err("not a Btor2 witness (no `sat` line)".into());
    }
    match lines.next() {
        Some(b) if b.starts_with('b') || b.starts_with('j') => {}
        _ => return Err("Btor2 witness without a property line".into()),
    }
    let assign = |l: &str,
                  decl: &[(String, u32)],
                  what: &str,
                  out: &mut BTreeMap<String, bool>|
     -> Result<(), String> {
        let mut w = l.split_whitespace();
        let idx: usize = w
            .next()
            .and_then(|i| i.parse().ok())
            .ok_or_else(|| format!("bad {what} line `{l}`"))?;
        let bits = w.next().ok_or_else(|| format!("bad {what} line `{l}`"))?;
        if bits.contains('[') {
            return Err("array witnesses are not supported".into());
        }
        let (name, width) = decl
            .get(idx)
            .ok_or_else(|| format!("{what} {idx} is not in the model"))?;
        if bits.len() != *width as usize {
            return Err(format!(
                "{what} {name}: {} bits for width {width}",
                bits.len()
            ));
        }
        for (k, c) in bits.chars().rev().enumerate() {
            let v = bit(c).ok_or_else(|| format!("bad bit `{c}` in `{l}`"))?;
            let key = if *width == 1 {
                name.clone()
            } else {
                format!("{name}[{k}]")
            };
            if let Some(v) = v {
                out.insert(key, v);
            } else if what == "input" {
                out.insert(key, false);
            }
        }
        Ok(())
    };
    let mut init = BTreeMap::new();
    let mut frames: Vec<BTreeMap<String, bool>> = Vec::new();
    // Which section the lines belong to: `#k` (states) or `@k` (inputs).
    let mut section: Option<(char, usize)> = None;
    for l in lines {
        if l == "." {
            break;
        }
        if let Some(k) = l.strip_prefix('#').or_else(|| l.strip_prefix('@')) {
            let k: usize = k.parse().map_err(|_| format!("bad frame line `{l}`"))?;
            let tag = l.chars().next().unwrap_or('#');
            if tag == '@' {
                if k != frames.len() {
                    return Err(format!("input frame @{k} out of order"));
                }
                frames.push(BTreeMap::new());
            }
            section = Some((tag, k));
            continue;
        }
        match section {
            Some(('#', 0)) => assign(l, &states, "state", &mut init)?,
            Some(('#', _)) => {}
            Some(('@', k)) => assign(l, &inputs, "input", &mut frames[k])?,
            _ => return Err(format!("assignment `{l}` outside a frame")),
        }
    }
    if frames.is_empty() {
        return Err("Btor2 witness without input frames".into());
    }
    Ok(render(&init, &frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2-bit counter with an enable, as an AIGER witness would see it:
    /// inputs `en`, `rst`; latches `q[0]`, `q[1]`.
    #[test]
    fn aiger_witness_to_named() {
        let out = "some log line\n1\nb0\n0x\n10\n10\n11\n.\nmore log\n";
        let w = extract(out, false).unwrap();
        assert_eq!(w, "1\nb0\n0x\n10\n10\n11\n.\n");
        let inputs = [
            Some("counter.en".to_string()),
            Some("counter.rst".to_string()),
        ];
        let latches = [
            Some("counter.q[0]".to_string()),
            Some("counter.q[1]".to_string()),
        ];
        let named = from_aiger(&w, &inputs, &latches).unwrap();
        assert_eq!(
            named,
            "informalw 1\ndepth 2\ninit counter.q[0]=0\n\
             @0 counter.en=1 counter.rst=0\n@1 counter.en=1 counter.rst=0\n@2 counter.en=1 counter.rst=1\n"
        );
        // Unnamed inputs are left out; size mismatches are refused.
        let named = from_aiger(&w, &[Some("counter.en".into()), None], &latches).unwrap();
        assert!(named.contains("@0 counter.en=1\n"));
        assert!(from_aiger(&w, &inputs, &latches[..1])
            .unwrap_err()
            .contains("1 latches"));
        assert!(from_aiger("1\nb0\n00\n1\n", &inputs, &latches)
            .unwrap_err()
            .contains("input values"));
        assert!(from_aiger("0\nb0\n", &inputs, &latches).is_err());
        assert!(extract("SAT\n", false).is_none());
    }

    const MODEL: &str =
        "1 sort bitvec 1\n2 sort bitvec 4\n3 input 1 counter.en\n4 input 1 counter.rst_n\n\
        5 state 2 counter.q\n6 state 1\n7 zero 2\n8 init 2 5 7\n";

    #[test]
    fn btor2_symbols_and_witness_to_named() {
        let (inputs, states) = btor2_symbols(MODEL);
        assert_eq!(
            inputs,
            [
                ("counter.en".to_string(), 1),
                ("counter.rst_n".to_string(), 1)
            ]
        );
        assert_eq!(
            states,
            [("counter.q".to_string(), 4), ("s6".to_string(), 1)]
        );
        let out = "[INFO] x\nSAT\nsat\nb0\n#0\n0 0000 counter.q\n1 1\n@0\n0 1\n1 0\n#1\n0 0000\n1 0\n@1\n0 1\n1 1\n.\n";
        let w = extract(out, true).unwrap();
        assert!(w.starts_with("sat\nb0\n#0\n") && w.ends_with(".\n"));
        let named = from_btor2(&w, MODEL).unwrap();
        assert_eq!(
            named,
            "informalw 1\ndepth 1\ninit counter.q[0]=0 counter.q[1]=0 counter.q[2]=0 counter.q[3]=0 s6=1\n\
             @0 counter.en=1 counter.rst_n=0\n@1 counter.en=1 counter.rst_n=1\n"
        );
        // MSB first: bit 0 is the last character.
        let one = from_btor2("sat\nb0\n#0\n0 0001\n@0\n0 0\n1 1\n.\n", MODEL).unwrap();
        assert!(one.contains("counter.q[0]=1 counter.q[1]=0"), "{one}");
        assert!(from_btor2("sat\nb0\n#0\n0 01\n@0\n.\n", MODEL)
            .unwrap_err()
            .contains("2 bits for width 4"));
        assert!(from_btor2("sat\nb0\n@0\n9 1\n.\n", MODEL)
            .unwrap_err()
            .contains("not in the model"));
        assert!(from_btor2("sat\nb0\n#0\n.\n", MODEL)
            .unwrap_err()
            .contains("without input frames"));
    }
}
