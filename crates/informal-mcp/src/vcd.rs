//! A counterexample or cover trace as data: the VCD the session wrote
//! (`informal_fpv::trace`, docs/ui/trace-format.md), read back into one
//! value per signal and frame.

use std::collections::HashMap;

/// One signal of a trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    /// Full name, scopes joined with dots (`fifo.u_wp.p`).
    pub name: String,
    pub width: usize,
    /// The clock lane (the trace's first variable).
    pub clock: bool,
    /// One value per frame: bits, most significant first; `x` when the
    /// trace gives none.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    /// Clock cycles, or model steps when `steps` is set.
    pub frames: usize,
    /// One frame is one model step (half a period of the fastest clock):
    /// a design on both clock edges or with several clocks.
    pub steps: bool,
    pub signals: Vec<Signal>,
    /// What the trace says about itself beyond the values (the loop of a
    /// liveness counterexample).
    pub notes: Vec<String>,
}

/// Read a VCD written by informal: values change at multiples of 10 time
/// units, one frame each; the clock's fall at 10f + 5 is not a frame.
pub fn parse(text: &str) -> Result<Trace, String> {
    let mut scope: Vec<&str> = Vec::new();
    let mut signals: Vec<Signal> = Vec::new();
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    let mut notes = Vec::new();
    let mut frames = None;
    let mut steps = false;
    let mut lines = text.lines();
    for l in lines.by_ref() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.first().copied() {
            Some("$comment") => {
                let body = w[1..w.len().saturating_sub(1)].join(" ");
                match body.strip_prefix("informal trace of ") {
                    // "<prop>: <n> cycles" or "<prop>: <n> model steps"
                    Some(rest) => {
                        let count = rest.rsplit(": ").next().unwrap_or("");
                        frames = count.split(' ').next().and_then(|n| n.parse().ok());
                        steps = count.ends_with("model steps");
                    }
                    None => notes.push(body),
                }
            }
            Some("$scope") => scope.push(w.get(2).copied().unwrap_or("")),
            Some("$upscope") => {
                scope.pop();
            }
            Some("$var") if w.len() >= 6 => {
                let width: usize = w[2].parse().map_err(|_| format!("bad $var: {l}"))?;
                by_id.insert(w[3], signals.len());
                signals.push(Signal {
                    name: scope
                        .iter()
                        .chain([&w[4]])
                        .copied()
                        .collect::<Vec<_>>()
                        .join("."),
                    width,
                    clock: signals.is_empty(),
                    values: Vec::new(),
                });
            }
            Some("$enddefinitions") => break,
            _ => {}
        }
    }
    // (frame, signal, bits) in file order.
    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut frame = None;
    let mut last = 0;
    for l in lines {
        if let Some(t) = l.strip_prefix('#') {
            let t: usize = t.trim().parse().map_err(|_| format!("bad time: {l}"))?;
            frame = t.is_multiple_of(10).then_some(t / 10);
            continue;
        }
        let Some(f) = frame else { continue };
        let (bits, id) = match l.strip_prefix('b') {
            Some(rest) => match rest.split_once(' ') {
                Some((bits, id)) => (bits, id.trim()),
                None => continue,
            },
            None if l.starts_with(['0', '1', 'x', 'z']) => l.split_at(1),
            None => continue,
        };
        if let Some(&i) = by_id.get(id) {
            changes.push((f, i, bits.to_string()));
            last = last.max(f);
        }
    }
    if signals.is_empty() {
        return Err("no signals in the trace".into());
    }
    let frames = frames.unwrap_or(last + 1);
    for s in &mut signals {
        s.values = vec!["x".repeat(s.width); frames];
    }
    // Carry each value forward to the frames after its change.
    for (f, i, bits) in changes {
        let s = &mut signals[i];
        let bits = format!("{bits:0>width$}", width = s.width);
        for v in s.values.iter_mut().skip(f) {
            v.clone_from(&bits);
        }
    }
    // A clock that rises with every cycle has nothing to say per frame.
    if !steps {
        signals.retain(|s| !s.clock);
    }
    Ok(Trace {
        frames,
        steps,
        signals,
        notes,
    })
}

/// Bits (most significant first) as the replies give them: one bit as it
/// is, a word in hex. A digit with an unknown bit is `x`.
pub fn value(bits: &str) -> String {
    if bits.len() == 1 {
        return bits.to_string();
    }
    let pad = (4 - bits.len() % 4) % 4;
    let padded = format!("{}{bits}", "0".repeat(pad));
    padded
        .as_bytes()
        .chunks(4)
        .map(|c| {
            let digit = c.iter().try_fold(0u32, |acc, b| match b {
                b'0' => Some(acc * 2),
                b'1' => Some(acc * 2 + 1),
                _ => None,
            });
            digit.and_then(|d| char::from_digit(d, 16)).unwrap_or('x')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VCD: &str = "\
$comment informal trace of a: 3 cycles $end
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$var reg 1 \" en $end
$var reg 4 # q [3:0] $end
$upscope $end
$scope module informal $end
$var reg 1 $ a $end
$upscope $end
$enddefinitions $end
#0
$dumpvars
1!
1\"
b1111 #
0$
$end
#5
0!
#10
1!
b0 #
#15
0!
#20
1!
0\"
b1010 #
1$
#25
0!
#30
";

    #[test]
    fn reads_frames_and_carries_values() {
        let t = parse(VCD).unwrap();
        assert_eq!((t.frames, t.steps), (3, false));
        let names: Vec<&str> = t.signals.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["top.en", "top.q", "informal.a"]);
        assert_eq!(t.signals[0].values, ["1", "1", "0"]);
        assert_eq!(t.signals[1].values, ["1111", "0000", "1010"]);
        assert_eq!(t.signals[2].values, ["0", "0", "1"]);
    }

    #[test]
    fn values_in_hex() {
        assert_eq!(value("1"), "1");
        assert_eq!(value("1010"), "a");
        assert_eq!(value("101111"), "2f");
        assert_eq!(value("1x0000"), "x0");
    }
}
