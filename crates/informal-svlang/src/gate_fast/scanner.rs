//! Byte-slice Verilog structural scanner and expression parser for `gate_fast`.

use super::parser::ParsedExpr;
use crate::struct_graph::normalize_sv_leaf;

pub(crate) fn parse_lhs_ref(s: &str) -> (String, Option<(u32, u32)>) {
    let t = s.trim();
    if t.is_empty() {
        return (String::new(), None);
    }
    if t.starts_with('\\') {
        return (normalize_sv_leaf(t), None);
    }
    if let Some(idx) = t.find('[') {
        let name = t[..idx].trim();
        let range = parse_bracket_range(&t[idx..]);
        (normalize_sv_leaf(name), range)
    } else {
        (normalize_sv_leaf(t), None)
    }
}

pub fn parse_expr_str(raw: &str) -> ParsedExpr {
    let s = raw.trim();
    if s.is_empty() {
        return ParsedExpr::Empty;
    }
    if let Some((is_zero, is_all_ones)) = try_parse_const(s) {
        return ParsedExpr::Const {
            is_zero,
            is_all_ones,
        };
    }
    if let Some(rest) = s.strip_prefix('~').or_else(|| s.strip_prefix('!')) {
        let rest = rest.trim();
        if is_simple_net_ref(rest) {
            let (net, range) = parse_lhs_ref(rest);
            return ParsedExpr::Ref {
                net,
                range,
                invert: true,
            };
        }
    }
    if is_simple_net_ref(s) {
        let (net, range) = parse_lhs_ref(s);
        return ParsedExpr::Ref {
            net,
            range,
            invert: false,
        };
    }
    let refs = extract_expr_refs(s);
    if refs.is_empty() {
        ParsedExpr::Empty
    } else {
        ParsedExpr::Compound { refs }
    }
}

fn is_simple_net_ref(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.starts_with('\\') {
        return !s.contains(['&', '|', '^', '+', '-', '?', ':', '{', '}']);
    }
    let bytes = s.as_bytes();
    if !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return false;
    }
    let mut i = 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$'))
    {
        i += 1;
    }
    if i == bytes.len() {
        return true;
    }
    bytes[i] == b'[' && bytes.last() == Some(&b']') && !s[i..].contains(['&', '|', '^', '{', '}'])
}

fn try_parse_const(s: &str) -> Option<(bool, bool)> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if t.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
        let clean: String = t.chars().filter(|&c| c != '_').collect();
        let v = clean.parse::<u128>().ok()?;
        return Some((v == 0, v == 1));
    }
    let (w_str, rest) = t.split_once('\'')?;
    let width = if w_str.is_empty() {
        1u32
    } else {
        w_str.trim().parse::<u32>().ok()?
    };
    let rest = rest.trim_start_matches(['s', 'S']);
    let mut chars = rest.chars();
    let base = chars.next()?.to_ascii_lowercase();
    let digits: String = chars.filter(|&c| c != '_' && !c.is_whitespace()).collect();
    if digits.is_empty() {
        return None;
    }
    match base {
        'b' => {
            let is_zero = digits.bytes().all(|b| b == b'0');
            let is_ones = digits.bytes().all(|b| b == b'1')
                && (digits.len() as u32 >= width || width == 1);
            Some((is_zero, is_ones))
        }
        'h' => {
            let is_zero = digits.bytes().all(|b| b == b'0');
            let is_ones = digits.bytes().all(|b| b == b'f' || b == b'F')
                && (digits.len() as u32 * 4 >= width);
            Some((is_zero, is_ones))
        }
        'd' => {
            let v = digits.parse::<u128>().ok()?;
            let all_ones = width < 128 && v == ((1u128 << width) - 1);
            Some((v == 0, all_ones))
        }
        _ => None,
    }
}

fn parse_bracket_range(s: &str) -> Option<(u32, u32)> {
    let inner = s.strip_prefix('[')?.strip_suffix(']')?.trim();
    if let Some((a, b)) = inner.split_once(':') {
        let hi = a.trim().parse::<u32>().ok()?;
        let lo = b.trim().parse::<u32>().ok()?;
        Some((lo.min(hi), lo.max(hi)))
    } else {
        let idx = inner.parse::<u32>().ok()?;
        Some((idx, idx))
    }
}

fn extract_expr_refs(s: &str) -> Vec<(String, Option<(u32, u32)>)> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            let start = i;
            i += 1;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let name = normalize_sv_leaf(&s[start..i]);
            if !name.is_empty() {
                out.push((name, None));
            }
            continue;
        }
        if b.is_ascii_digit() {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'\'' | b'_'))
            {
                i += 1;
            }
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$'))
            {
                i += 1;
            }
            let ident = &s[start..i];
            let mut range = None;
            let mut k = i;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < bytes.len() && bytes[k] == b'[' {
                if let Some(rel_end) = s[k..].find(']') {
                    range = parse_bracket_range(&s[k..=k + rel_end]);
                    i = k + rel_end + 1;
                }
            }
            out.push((ident.to_string(), range));
            continue;
        }
        i += 1;
    }
    out
}

pub(crate) struct Scanner<'a> {
    file: &'a str,
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    pub(crate) line: u32,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(src: &'a str, file: &'a str) -> Self {
        Self {
            file,
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
        }
    }

    pub(crate) fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub(crate) fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    pub(crate) fn starts_with_bytes(&self, pat: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(pat)
    }

    pub(crate) fn advance_one(&mut self) {
        if self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'\n' {
                self.line += 1;
            }
            self.pos += 1;
        }
    }

    pub(crate) fn err(&self, msg: &str) -> String {
        format!("gate_fast: {}:{}: {msg}", self.file, self.line)
    }

    pub(crate) fn skip_ws_and_comments(&mut self) {
        loop {
            while self.pos < self.bytes.len() {
                match self.bytes[self.pos] {
                    b' ' | b'\t' | b'\r' | 0x0b | 0x0c => self.pos += 1,
                    b'\n' => {
                        self.line += 1;
                        self.pos += 1;
                    }
                    _ => break,
                }
            }
            if self.pos + 1 < self.bytes.len() && self.bytes[self.pos] == b'/' {
                if self.bytes[self.pos + 1] == b'/' {
                    self.pos += 2;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    continue;
                }
                if self.bytes[self.pos + 1] == b'*' {
                    self.pos += 2;
                    while self.pos + 1 < self.bytes.len() {
                        if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                            self.pos += 2;
                            break;
                        }
                        if self.bytes[self.pos] == b'\n' {
                            self.line += 1;
                        }
                        self.pos += 1;
                    }
                    continue;
                }
            }
            break;
        }
    }

    pub(crate) fn skip_directive_line(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    pub(crate) fn skip_attr(&mut self) {
        self.pos += 2;
        while self.pos + 1 < self.bytes.len() {
            if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b')' {
                self.pos += 2;
                return;
            }
            if self.bytes[self.pos] == b'\n' {
                self.line += 1;
            }
            self.pos += 1;
        }
    }

    pub(crate) fn read_ident(&mut self) -> Option<&'a str> {
        self.skip_ws_and_comments();
        let b = self.peek_byte()?;
        if b == b'\\' {
            let start = self.pos;
            self.pos += 1;
            while self.pos < self.bytes.len() && !self.bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            return Some(&self.src[start..self.pos]);
        }
        if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            let start = self.pos;
            self.pos += 1;
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_alphanumeric()
                    || matches!(self.bytes[self.pos], b'_' | b'$'))
            {
                self.pos += 1;
            }
            return Some(&self.src[start..self.pos]);
        }
        None
    }

    pub(crate) fn read_range_spec(&mut self) -> Option<(u32, (i128, i128))> {
        let inner = self.read_balanced_inner(b'[', b']');
        let (a, b) = inner.split_once(':')?;
        let l = a.trim().parse::<i128>().ok()?;
        let r = b.trim().parse::<i128>().ok()?;
        let w = (l - r).unsigned_abs() as u32 + 1;
        Some((w.max(1), (l, r)))
    }

    pub(crate) fn skip_balanced(&mut self, open: u8, close: u8) {
        let _ = self.read_balanced_inner(open, close);
    }

    pub(crate) fn read_balanced_inner(&mut self, open: u8, close: u8) -> &'a str {
        if self.peek_byte() != Some(open) {
            return "";
        }
        self.advance_one();
        let start = self.pos;
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'\n' {
                self.line += 1;
                self.pos += 1;
            } else if b == b'"' {
                self.pos += 1;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'"' {
                    if self.bytes[self.pos] == b'\n' {
                        self.line += 1;
                    }
                    self.pos += 1;
                }
                if self.pos < self.bytes.len() {
                    self.pos += 1;
                }
            } else if b == open {
                depth += 1;
                self.pos += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    let end = self.pos;
                    self.pos += 1;
                    return &self.src[start..end];
                }
                self.pos += 1;
            } else {
                self.pos += 1;
            }
        }
        &self.src[start..self.pos]
    }

    pub(crate) fn read_until_semi(&mut self) -> &'a str {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b';' {
                let end = self.pos;
                self.pos += 1;
                return &self.src[start..end];
            }
            if b == b'\n' {
                self.line += 1;
            }
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    pub(crate) fn read_until_comma_or_semi(&mut self) -> &'a str {
        let start = self.pos;
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b'(' | b'{' | b'[' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' | b'}' | b']' => {
                    depth = depth.saturating_sub(1);
                    self.pos += 1;
                }
                b',' | b';' if depth == 0 => {
                    return &self.src[start..self.pos];
                }
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        &self.src[start..self.pos]
    }

    pub(crate) fn read_until_comma_or_rparen(&mut self) -> &'a str {
        let start = self.pos;
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b'(' | b'{' | b'[' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' if depth == 0 => {
                    return &self.src[start..self.pos];
                }
                b')' | b'}' | b']' => {
                    depth = depth.saturating_sub(1);
                    self.pos += 1;
                }
                b',' if depth == 0 => {
                    return &self.src[start..self.pos];
                }
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        &self.src[start..self.pos]
    }

    pub(crate) fn skip_to_semi_or_eof(&mut self) {
        let _ = self.read_until_semi();
    }

    pub(crate) fn skip_until_keyword(&mut self, end_kw: &str) {
        while !self.eof() {
            self.skip_ws_and_comments();
            if let Some(w) = self.read_ident() {
                if w == end_kw {
                    return;
                }
            } else {
                self.advance_one();
            }
        }
    }
}
