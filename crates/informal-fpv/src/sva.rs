//! SVA subset compiler.
//!
//! Lowers the fixed-delay fragment of SVA, which covers most hand-written
//! FPV checks, to immediate assertions over `$past`:
//!
//!   [@(posedge clk)] [disable iff (d)] seq [ |-> | |=> seq ]
//!   seq := [##N] b {##N b}
//!
//! where each `b` is a boolean SystemVerilog expression (which may itself
//! use `$past`, `$rose`, `$fell`, `$stable`, `$changed`). A property whose
//! sequences span T cycles is checked at the cycle its last term lands on,
//! by looking back with `$past(b, k)`. Ranges (`##[a:b]`), repetition,
//! `throughout`, `until`, `s_eventually` and local variables are outside
//! this subset. A property that only adds bounded sequence operators goes
//! to svlang's property compiler instead ([`needs_compiler`]), which will
//! replace this module once every command property goes through it.

/// One boolean term of a sequence, `offset` cycles after the sequence start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub offset: u32,
    pub expr: String,
}

/// A clock edge named in `@(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockSpec {
    pub edge: &'static str,
    pub signal: String,
}

/// A property after parsing, with every term placed on one time line that
/// starts at the antecedent's first cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lowered {
    pub clock: Option<ClockSpec>,
    pub disable: Option<String>,
    /// Terms that enable the check (empty for a plain sequence).
    pub antecedent: Vec<Term>,
    /// Terms that must hold (or, for a cover, match).
    pub consequent: Vec<Term>,
    /// Cycle of the last term: the check fires `span` cycles after the start.
    pub span: u32,
}

const UNSUPPORTED: &[&str] = &[
    "##[",
    "[*",
    "[->",
    "[=",
    "[+]",
    "throughout",
    "within",
    "intersect",
    "first_match",
    "s_eventually",
    "eventually",
    "s_until_with",
    "s_until",
    "until_with",
    "until",
    "s_nexttime",
    "nexttime",
    "s_always",
    "always",
    "strong(",
    "weak(",
    "accept_on",
    "reject_on",
    "#-#",
    "#=#",
    "not ",
];

/// The operators of [`UNSUPPORTED`] that svlang's property compiler takes
/// when their ranges are bounded.
const COMPILED: &[&str] = &[
    "##[",
    "[*",
    "throughout",
    "within",
    "intersect",
    "first_match",
    "until_with",
    "until",
    "s_eventually",
];

/// True for a property this module cannot lower but svlang's compiler may:
/// every operator outside the fixed-delay subset that it uses is one of
/// the bounded sequence operators. The session then hands the property to
/// the front end, which binds it in the design and says exactly what it
/// cannot compile.
pub fn needs_compiler(src: &str) -> bool {
    let used: Vec<&&str> = UNSUPPORTED
        .iter()
        .filter(|w| contains_word(src, w))
        .collect();
    !used.is_empty() && used.iter().all(|w| COMPILED.contains(w))
}

pub fn parse(src: &str) -> Result<Lowered, String> {
    let mut s = src.trim();
    let mut clock = None;
    let mut disable = None;
    if let Some(rest) = s.strip_prefix('@') {
        let (inner, rest) = paren_group(rest.trim_start())
            .ok_or_else(|| format!("bad clocking event in `{src}`"))?;
        clock = Some(parse_clock(inner)?);
        s = rest.trim_start();
    }
    if let Some(rest) = s.strip_prefix("disable") {
        let rest = rest
            .trim_start()
            .strip_prefix("iff")
            .ok_or_else(|| format!("expected `disable iff` in `{src}`"))?;
        let (inner, rest) = paren_group(rest.trim_start())
            .ok_or_else(|| format!("bad `disable iff` condition in `{src}`"))?;
        disable = Some(inner.trim().to_string());
        s = rest.trim_start();
    }
    let body = strip_outer_parens(s);
    if body.is_empty() {
        return Err("empty property".into());
    }
    for word in UNSUPPORTED {
        if contains_word(body, word) {
            return Err(format!(
                "`{}` is not supported yet by the SVA subset (fixed ## delays, |->, |=>)",
                word.trim()
            ));
        }
    }
    let (antecedent, consequent, span) = match split_implication(body) {
        Some((ante, overlap, cons)) => {
            let (a, a_len) = sequence(ante, 0)?;
            let start = a_len + u32::from(!overlap);
            let (c, c_len) = sequence(cons, start)?;
            (a, c, c_len)
        }
        None => {
            let (c, len) = sequence(body, 0)?;
            (Vec::new(), c, len)
        }
    };
    Ok(Lowered {
        clock,
        disable,
        antecedent,
        consequent,
        span,
    })
}

fn parse_clock(inner: &str) -> Result<ClockSpec, String> {
    let words: Vec<&str> = inner.split_whitespace().collect();
    match words.as_slice() {
        ["posedge", sig] => Ok(ClockSpec {
            edge: "posedge",
            signal: sig.to_string(),
        }),
        ["negedge", sig] => Ok(ClockSpec {
            edge: "negedge",
            signal: sig.to_string(),
        }),
        [sig] => Ok(ClockSpec {
            edge: "posedge",
            signal: sig.to_string(),
        }),
        _ => Err(format!("unsupported clocking event `@({inner})`")),
    }
}

/// `(...)rest` -> (inside, rest), honouring nesting.
fn paren_group(s: &str) -> Option<(&str, &str)> {
    if !s.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_outer_parens(mut s: &str) -> &str {
    loop {
        s = s.trim();
        match paren_group(s) {
            Some((inner, rest)) if rest.trim().is_empty() => s = inner,
            _ => return s,
        }
    }
}

/// Byte offsets of `pat` at nesting depth 0.
fn top_level(s: &str, pat: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if depth == 0 && s[i..].starts_with(pat) => {
                out.push(i);
                i += pat.len();
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn contains_word(s: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    s.match_indices(word).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + word.len()..].chars().next();
        let w0 = word.chars().next().is_some_and(ident);
        let w1 = word.chars().next_back().is_some_and(ident);
        (!w0 || !before.is_some_and(ident)) && (!w1 || !after.is_some_and(ident))
    })
}

/// `a |-> b` -> (a, overlapping, b).
fn split_implication(s: &str) -> Option<(&str, bool, &str)> {
    let ov = top_level(s, "|->");
    let nov = top_level(s, "|=>");
    let (at, overlap) = match (ov.first(), nov.first()) {
        (Some(&a), Some(&b)) => (a.min(b), a < b),
        (Some(&a), None) => (a, true),
        (None, Some(&b)) => (b, false),
        (None, None) => return None,
    };
    Some((&s[..at], overlap, &s[at + 3..]))
}

/// Terms of `[##N] b {##N b}`, placed from `start`; also returns the offset
/// of the last term.
fn sequence(s: &str, start: u32) -> Result<(Vec<Term>, u32), String> {
    let s = strip_outer_parens(s);
    let cuts = top_level(s, "##");
    let mut pieces = Vec::new();
    let mut prev = 0;
    for c in cuts {
        pieces.push(&s[prev..c]);
        prev = c + 2;
    }
    pieces.push(&s[prev..]);

    let mut terms = Vec::new();
    let mut at = start;
    for (i, piece) in pieces.iter().enumerate() {
        let mut p = piece.trim();
        if i > 0 {
            let digits = p.len() - p.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if digits == 0 {
                return Err(format!("expected a delay after `##` in `{s}`"));
            }
            at += p[..digits].parse::<u32>().map_err(|e| e.to_string())?;
            p = p[digits..].trim();
        } else if p.is_empty() {
            continue; // leading `##N`
        }
        let expr = strip_outer_parens(p);
        if expr.is_empty() {
            return Err(format!("missing expression in sequence `{s}`"));
        }
        terms.push(Term {
            offset: at,
            expr: expr.to_string(),
        });
    }
    if terms.is_empty() {
        return Err(format!("empty sequence `{s}`"));
    }
    Ok((terms, at))
}

/// How many cycles back `expr` reads through sampled-value functions:
/// `$past(e, k)` reads k (default 1) cycles before what `e` reads, and
/// `$rose`, `$fell`, `$stable` and `$changed` one. 0 without any.
pub fn lookback(expr: &str) -> u32 {
    const CALLS: [&str; 5] = ["$past", "$rose", "$fell", "$stable", "$changed"];
    let b = expr.as_bytes();
    let mut deepest = 0;
    let mut i = 0;
    while let Some(at) = expr[i..].find('$').map(|j| i + j) {
        i = at + 1;
        let Some(call) = CALLS.iter().find(|c| expr[at..].starts_with(**c)) else {
            continue;
        };
        let mut open = at + call.len();
        if b.get(open)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            continue; // a longer name, e.g. $past_gclk
        }
        while b.get(open).is_some_and(u8::is_ascii_whitespace) {
            open += 1;
        }
        if b.get(open) != Some(&b'(') {
            continue;
        }
        // The arguments, split at top-level commas.
        let (mut depth, mut start, mut args) = (0u32, open + 1, Vec::new());
        let mut end = expr.len();
        for (j, c) in expr[open..].char_indices().map(|(j, c)| (open + j, c)) {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
                ',' if depth == 1 => {
                    args.push(&expr[start..j]);
                    start = j + 1;
                }
                _ => {}
            }
        }
        args.push(&expr[start..end.max(start)]);
        let k = if *call == "$past" {
            args.get(1)
                .and_then(|a| a.trim().parse::<u32>().ok())
                .unwrap_or(1)
        } else {
            1
        };
        // The scan goes on inside the arguments, so add theirs here.
        deepest = deepest.max(k + lookback(args[0]));
    }
    deepest
}

/// `$past(e, k)`, or `e` itself for k = 0.
pub fn past(expr: &str, k: u32) -> String {
    if k == 0 {
        format!("({expr})")
    } else if k == 1 {
        format!("$past({expr})")
    } else {
        format!("$past({expr}, {k})")
    }
}

impl Lowered {
    /// The first cycle at which everything the check reads is real: each
    /// term looks back to its own cycle, and from there through any `$past`,
    /// `$rose`, `$fell`, `$stable` or `$changed` inside it. Before that the
    /// history registers hold no sampled value yet (as in JasperGold, the
    /// check waits for them rather than reading an arbitrary one).
    pub fn history(&self) -> u32 {
        let terms = self.antecedent.iter().chain(&self.consequent);
        let deepest = terms
            .map(|t| self.span - t.offset + lookback(&t.expr))
            .max()
            .unwrap_or(0);
        let disable = self
            .disable
            .as_deref()
            .map_or(0, |d| self.span + lookback(d));
        deepest.max(disable)
    }

    fn conj(&self, terms: &[Term]) -> String {
        if terms.is_empty() {
            return "1'b1".into();
        }
        terms
            .iter()
            .map(|t| past(&t.expr, self.span - t.offset))
            .collect::<Vec<_>>()
            .join(" && ")
    }

    /// Antecedent as a boolean at the check cycle (`1'b1` when absent).
    pub fn enable(&self) -> String {
        self.conj(&self.antecedent)
    }

    /// Consequent as a boolean at the check cycle.
    pub fn body(&self) -> String {
        self.conj(&self.consequent)
    }

    /// `disable iff` must be false on every cycle of the attempt.
    pub fn not_disabled(&self) -> Option<String> {
        self.disable.as_ref().map(|d| {
            (0..=self.span)
                .map(|k| format!("!{}", past(d, k)))
                .collect::<Vec<_>>()
                .join(" && ")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_counts_sampled_value_functions() {
        assert_eq!(lookback("a && b"), 0);
        assert_eq!(lookback("$rose(req)"), 1);
        assert_eq!(lookback("x == $past(y, 3) || $stable(z)"), 3);
        assert_eq!(lookback("$past($rose(a), 2)"), 3);
        assert_eq!(lookback("$past(f(a, b))"), 1);
        assert_eq!(parse("req |-> gnt").unwrap().history(), 0);
        assert_eq!(parse("$rose(req) |-> gnt").unwrap().history(), 1);
        // The antecedent is two cycles back at the check, and reads one more.
        assert_eq!(parse("$rose(req) |-> ##2 gnt").unwrap().history(), 3);
        assert_eq!(parse("req |=> $past(d, 2) == q").unwrap().history(), 2);
    }

    #[test]
    fn plain_boolean() {
        let l = parse("q <= 9").unwrap();
        assert_eq!(l.span, 0);
        assert_eq!(l.enable(), "1'b1");
        assert_eq!(l.body(), "(q <= 9)");
    }

    #[test]
    fn overlapping_and_next_cycle_implication() {
        let l = parse("@(posedge clk) disable iff (!rst_n) req |-> ##2 ack").unwrap();
        assert_eq!(l.clock.as_ref().unwrap().signal, "clk");
        assert_eq!(l.span, 2);
        assert_eq!(l.enable(), "$past(req, 2)");
        assert_eq!(l.body(), "(ack)");
        assert_eq!(
            l.not_disabled().unwrap(),
            "!(!rst_n) && !$past(!rst_n) && !$past(!rst_n, 2)"
        );

        let l = parse("(a ##1 b) |=> (c ##1 (d || e))").unwrap();
        assert_eq!(l.span, 3);
        assert_eq!(l.enable(), "$past(a, 3) && $past(b, 2)");
        assert_eq!(l.body(), "$past(c) && (d || e)");
    }

    #[test]
    fn nested_operators_stay_inside_terms() {
        let l = parse("$rose(start) |-> busy[0] || (cnt == {2'b0, x})").unwrap();
        assert_eq!(l.enable(), "($rose(start))");
        assert_eq!(l.body(), "(busy[0] || (cnt == {2'b0, x}))");
    }

    #[test]
    fn unsupported_constructs_are_named() {
        assert!(parse("a |-> ##[1:3] b").unwrap_err().contains("##["));
        assert!(parse("a |-> s_eventually b")
            .unwrap_err()
            .contains("s_eventually"));
        assert!(parse("a[*2] |-> b").unwrap_err().contains("[*"));
        // Identifiers that merely contain a keyword are fine.
        assert!(parse("always_on |-> until_done").is_ok());
    }
}
