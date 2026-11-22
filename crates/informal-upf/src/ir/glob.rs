//! Tcl `string match`-style glob matcher and RTL bracket escaping helpers.

/// Pure-Rust Tcl `string match`-style glob matcher supporting `*`, `?`, `[...]`
/// character classes, and `\` escapes.
pub fn glob(pat: &str, s: &str) -> bool {
    fn match_class(pat: &[u8], ch: u8) -> Option<(bool, usize)> {
        if pat.first() != Some(&b'[') {
            return None;
        }
        let mut i = 1;
        let negated = pat.get(i).is_some_and(|&c| c == b'!' || c == b'^');
        if negated {
            i += 1;
        }
        let mut matched = false;
        let mut prev: Option<u8> = None;
        while i < pat.len() {
            if pat[i] == b'/' || pat[i] == b'[' {
                return None;
            }
            if pat[i] == b']' && (i > 1 + usize::from(negated)) {
                return Some((matched ^ negated, i + 1));
            }
            if pat[i] == b'-' && i + 1 < pat.len() && pat[i + 1] != b']' {
                if let Some(lo) = prev {
                    if pat[i + 1] == b'/' || pat[i + 1] == b'[' {
                        return None;
                    }
                    let (hi, next_i) = if pat[i + 1] == b'\\' && i + 2 < pat.len() {
                        (pat[i + 2], i + 3)
                    } else {
                        (pat[i + 1], i + 2)
                    };
                    if lo <= ch && ch <= hi {
                        matched = true;
                    }
                    prev = None;
                    i = next_i;
                    continue;
                }
            }
            let c = if pat[i] == b'\\' && i + 1 < pat.len() {
                i += 1;
                pat[i]
            } else {
                pat[i]
            };
            if c == ch {
                matched = true;
            }
            prev = Some(c);
            i += 1;
        }
        None
    }

    let p = pat.as_bytes();
    let s = s.as_bytes();
    let mut pi = 0usize;
    let mut si = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_si = 0usize;

    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            while pi < p.len() && p[pi] == b'*' {
                pi += 1;
            }
            star_pi = Some(pi);
            star_si = si;
            continue;
        }
        if pi < p.len() {
            match p[pi] {
                b'?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                b'\\' if pi + 1 < p.len() => {
                    if p[pi + 1] == s[si] {
                        pi += 2;
                        si += 1;
                        continue;
                    }
                }
                b'[' => {
                    if let Some((ok, consumed)) = match_class(&p[pi..], s[si]) {
                        if ok {
                            pi += consumed;
                            si += 1;
                            continue;
                        }
                    } else if p[pi] == s[si] {
                        pi += 1;
                        si += 1;
                        continue;
                    }
                }
                a if a == s[si] => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(sp) = star_pi {
            star_si += 1;
            si = star_si;
            pi = sp;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Escapes all Tcl glob metacharacters (`*`, `?`, `[`, `]`, `\`) for literal matching.
pub fn escape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Escapes unescaped `[` and `]` in a pattern while preserving `*`, `?`, and existing `\` escapes.
pub fn escape_glob_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push('\\');
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else if ch == '[' || ch == ']' {
            out.push('\\');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

enum BracketKind {
    Empty,
    RtlIndex(usize),
    CharClass(usize),
    Unclosed,
}

fn classify_bracket(pat: &str, open_idx: usize) -> BracketKind {
    let bytes = pat.as_bytes();
    if open_idx + 1 < bytes.len() && bytes[open_idx + 1] == b']' {
        return BracketKind::Empty;
    }
    let mut j = open_idx + 1;
    let mut escaped = false;
    let mut close_idx = None;
    while j < bytes.len() {
        if escaped {
            escaped = false;
            j += pat[j..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        if bytes[j] == b'\\' {
            escaped = true;
            j += 1;
            continue;
        }
        if bytes[j] == b'/' || bytes[j] == b'[' {
            break;
        }
        if bytes[j] == b']' {
            close_idx = Some(j);
            break;
        }
        j += pat[j..].chars().next().map_or(1, char::len_utf8);
    }
    let Some(end) = close_idx else {
        return BracketKind::Unclosed;
    };
    let inner = &pat[open_idx + 1..end];
    let is_rtl_slice = inner.split_once(':').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
    });
    let is_rtl_index = !inner.is_empty()
        && inner.bytes().all(|c| c.is_ascii_digit())
        && (inner.len() == 1 || !inner.starts_with('0'));
    if is_rtl_slice || is_rtl_index {
        BracketKind::RtlIndex(end + 1)
    } else {
        BracketKind::CharClass(end + 1)
    }
}

pub(crate) fn closed_bracket_kind(pat: &str, open_idx: usize) -> Option<(usize, bool)> {
    match classify_bracket(pat, open_idx) {
        BracketKind::Empty => Some((open_idx + 2, false)),
        BracketKind::RtlIndex(next) => Some((next, false)),
        BracketKind::CharClass(next) => Some((next, true)),
        BracketKind::Unclosed => None,
    }
}

/// Escapes `[` and `]` only on bracket groups that look like RTL bit/array indices
/// (`[0]`, `[12]`, `[3:0]`), plus empty `[]` and unclosed `[` (before `/`, `[`, or EOF),
/// leaving real Tcl glob character classes (`[0-1]`, `[!0]`, `[ab]`, `[01]`) and existing
/// backslash escapes intact.
pub fn escape_rtl_brackets(pat: &str) -> String {
    let bytes = pat.as_bytes();
    let mut out = String::with_capacity(pat.len() + 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            out.push('\\');
            i += 1;
            if i < bytes.len() {
                if let Some(ch) = pat[i..].chars().next() {
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            continue;
        }
        if bytes[i] == b'[' {
            match classify_bracket(pat, i) {
                BracketKind::Empty => {
                    out.push_str("\\[\\]");
                    i += 2;
                }
                BracketKind::RtlIndex(next) => {
                    out.push_str("\\[");
                    out.push_str(&pat[i + 1..next - 1]);
                    out.push_str("\\]");
                    i = next;
                }
                BracketKind::CharClass(next) => {
                    out.push_str(&pat[i..next]);
                    i = next;
                }
                BracketKind::Unclosed => {
                    out.push_str("\\[");
                    i += 1;
                }
            }
            continue;
        }
        if let Some(ch) = pat[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

/// Returns `true` if `pat` contains an unescaped Tcl glob character class `[...]`
/// (ranges like `[0-1]`, negations `[!0]`/`[^0]`, or multi-char/non-numeric classes like `[ab]`/`[01]`),
/// as opposed to RTL bit/array indices like `[0]`, `[12]`, or slices `[3:0]`.
pub fn is_glob_char_class(pat: &str) -> bool {
    let bytes = pat.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                if let Some(ch) = pat[i..].chars().next() {
                    i += ch.len_utf8();
                }
            }
            continue;
        }
        if bytes[i] == b'[' {
            match classify_bracket(pat, i) {
                BracketKind::CharClass(_) => return true,
                BracketKind::Empty => {
                    i += 2;
                    continue;
                }
                BracketKind::RtlIndex(next) => {
                    i = next;
                    continue;
                }
                BracketKind::Unclosed => {
                    i += 1;
                    continue;
                }
            }
        }
        if let Some(ch) = pat[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    false
}

/// Returns `true` if `pat` contains an unescaped `*`, `?`, or Tcl glob character class `[...]`.
pub(crate) fn has_unescaped_glob_wildcards(pat: &str) -> bool {
    let bytes = pat.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < bytes.len() {
                if let Some(ch) = pat[i..].chars().next() {
                    i += ch.len_utf8();
                }
            }
            continue;
        }
        if bytes[i] == b'*' || bytes[i] == b'?' {
            return true;
        }
        if bytes[i] == b'[' {
            match classify_bracket(pat, i) {
                BracketKind::CharClass(_) => return true,
                BracketKind::Empty => {
                    i += 2;
                    continue;
                }
                BracketKind::RtlIndex(next) => {
                    i = next;
                    continue;
                }
                BracketKind::Unclosed => {
                    i += 1;
                    continue;
                }
            }
        }
        if let Some(ch) = pat[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    false
}

/// Escapes the leading `\` of SystemVerilog escaped identifiers (e.g. `\u:esc /*`
/// -> `\\u:esc /*`) before passing a pattern to [`glob`], while preserving
/// Tcl glob escapes (`\*`, `\?`, `\[`, `\]`, `\\`, and `\[...\]`).
pub(crate) fn escape_sv_escaped_backslashes_for_glob(pat: &str) -> String {
    let bytes = pat.as_bytes();
    let mut out = String::with_capacity(pat.len() + 4);
    let mut i = 0usize;
    let mut at_seg_start = true;
    while i < bytes.len() {
        if at_seg_start && bytes[i] == b'\\' {
            if let Some(end) = super::path::scan_sv_escaped_ident_end(pat, i) {
                out.push_str("\\\\");
                out.push_str(&pat[i + 1..end]);
                at_seg_start = pat[i..end].chars().last().is_some_and(char::is_whitespace);
                i = end;
                continue;
            }
        }
        if bytes[i] == b'\\' {
            out.push('\\');
            i += 1;
            if i < bytes.len() {
                if let Some(ch) = pat[i..].chars().next() {
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            at_seg_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(pat, i) {
                out.push_str(&pat[i..end]);
                i = end;
                at_seg_start = is_char_class;
                continue;
            }
        }
        if let Some(ch) = pat[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
            at_seg_start = matches!(ch, '/' | ':' | '.' | '*' | '?') || ch.is_whitespace();
        } else {
            break;
        }
    }
    out
}

/// Unescapes pre-escaped `\[` and `\]` while preserving other backslashes
/// (such as SystemVerilog escaped identifiers `\u_esc `).
pub(crate) fn unescape_rtl_brackets(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 1 < bytes.len() && (bytes[i + 1] == b'[' || bytes[i + 1] == b']') {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            out.push('\\');
            i += 1;
            if i < bytes.len() {
                if let Some(ch) = s[i..].chars().next() {
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            continue;
        }
        if let Some(ch) = s[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}
