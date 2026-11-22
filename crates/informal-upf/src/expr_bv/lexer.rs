//! Span-aware SystemVerilog expression lexer (`D1` + `Fix 8`) and scope qualifier.

use std::ops::Range;

use crate::ir::{normalize_scope, split_hier_segments};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdentSeg {
    pub name: String,
    pub is_esc: bool,
    pub index: Option<String>,
}

impl IdentSeg {
    pub(crate) fn push_formatted(&self, out: &mut String, has_next: bool) {
        if self.is_esc {
            out.push('\\');
        }
        out.push_str(&self.name);
        if let Some(idx) = &self.index {
            if self.is_esc {
                out.push(' ');
            }
            out.push_str(idx);
        } else if self.is_esc && has_next {
            out.push(' ');
        }
    }

    pub(crate) fn as_ast_part(&self) -> String {
        match &self.index {
            Some(idx) if self.is_esc => format!("\\{} {idx}", self.name),
            Some(idx) => format!("{}{idx}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IdentToken {
    pub is_abs: bool,
    pub segments: Vec<IdentSeg>,
}

impl IdentToken {
    pub(crate) fn as_path_string(&self) -> String {
        let joined = self
            .segments
            .iter()
            .map(IdentSeg::as_ast_part)
            .collect::<Vec<_>>()
            .join("/");
        if self.is_abs {
            format!("/{joined}")
        } else {
            joined
        }
    }

    pub(crate) fn is_bool_literal(&self) -> Option<bool> {
        if !self.is_abs
            && self.segments.len() == 1
            && !self.segments[0].is_esc
            && self.segments[0].index.is_none()
        {
            let s = &self.segments[0].name;
            if s.eq_ignore_ascii_case("true") {
                return Some(true);
            }
            if s.eq_ignore_ascii_case("false") {
                return Some(false);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ExprTok {
    Ident(IdentToken),
    Number(String),
    BraceGroup(String),
    LBracket,
    RBracket,
    Colon,
    LParen,
    RParen,
    Question,
    Bang,
    Tilde,
    And,
    Or,
    Xor,
    Xnor,
    Nand,
    Nor,
    AndAnd,
    OrOr,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    Shl,
    Shr,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn has_following_colon_for_ternary(input: &str, start: usize) -> bool {
    let bytes = input.as_bytes();
    let mut j = start;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut local_ternary = 0usize;
    let mut seen_colons = 0usize;
    while j < bytes.len() {
        let b = bytes[j];
        if b == b'\\' {
            j += 1;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            continue;
        }
        match b {
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'(' => paren_depth += 1,
            b')' => {
                if paren_depth == 0 {
                    break;
                }
                paren_depth -= 1;
            }
            b'?' if paren_depth == 0 && bracket_depth == 0 => {
                local_ternary += 1;
            }
            b':' if paren_depth == 0 && bracket_depth == 0 => {
                if local_ternary > 0 {
                    local_ternary -= 1;
                } else {
                    seen_colons += 1;
                    if seen_colons >= 2 {
                        return true;
                    }
                }
            }
            _ => {}
        }
        j += 1;
    }
    false
}

fn scan_hier_bracket_index(
    input: &str,
    bracket_pos: usize,
    sep: char,
    allow_colon_sep: bool,
) -> Option<(usize, usize, bool)> {
    let bytes = input.as_bytes();
    if bytes.get(bracket_pos) != Some(&b'[') {
        return None;
    }
    let mut k = bracket_pos + 1;
    let first_start = k;
    while k < bytes.len() && bytes[k].is_ascii_digit() {
        k += 1;
    }
    if k == first_start {
        return None;
    }
    if bytes.get(k) == Some(&b':') {
        k += 1;
        let second_start = k;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k == second_start {
            return None;
        }
    }
    if bytes.get(k) != Some(&b']') {
        return None;
    }
    let after_bracket = k + 1;
    let is_sep_byte = |c: u8| c == b'/' || (sep == ':' && allow_colon_sep && c == b':');
    if after_bracket + 1 < bytes.len()
        && is_sep_byte(bytes[after_bracket])
        && (bytes[after_bracket + 1] == b'\\'
            || bytes[after_bracket + 1] == b'.'
            || is_ident_start(bytes[after_bracket + 1]))
    {
        return Some((after_bracket, after_bracket + 1, true));
    }
    if after_bracket + 1 < bytes.len()
        && bytes[after_bracket] == b'.'
        && (bytes[after_bracket + 1] == b'\\' || is_ident_start(bytes[after_bracket + 1]))
    {
        return Some((after_bracket, after_bracket + 1, false));
    }
    None
}

fn scan_ident_token_with_sep(
    input: &str,
    start: usize,
    sep: char,
    allow_colon_sep: bool,
) -> Result<(IdentToken, usize), String> {
    let bytes = input.as_bytes();
    let mut i = start;
    let mut is_abs = false;
    let is_sep_byte = |c: u8| c == b'/' || (sep == ':' && allow_colon_sep && c == b':');
    if i < bytes.len() && is_sep_byte(bytes[i]) {
        is_abs = true;
        i += 1;
    }
    let mut segments = Vec::new();
    let mut at_seg_start = true;
    loop {
        if at_seg_start
            && i + 2 < bytes.len()
            && bytes[i] == b'.'
            && bytes[i + 1] == b'.'
            && is_sep_byte(bytes[i + 2])
            && bytes
                .get(i + 3)
                .is_some_and(|&c| c == b'\\' || c == b'.' || is_ident_start(c))
        {
            segments.push(IdentSeg {
                name: "..".to_string(),
                is_esc: false,
                index: None,
            });
            i += 3;
            at_seg_start = true;
            continue;
        }
        if at_seg_start
            && i + 1 < bytes.len()
            && bytes[i] == b'.'
            && is_sep_byte(bytes[i + 1])
            && bytes
                .get(i + 2)
                .is_some_and(|&c| c == b'\\' || c == b'.' || is_ident_start(c))
        {
            segments.push(IdentSeg {
                name: ".".to_string(),
                is_esc: false,
                index: None,
            });
            i += 2;
            at_seg_start = true;
            continue;
        }
        if i < bytes.len() && bytes[i] == b'\\' {
            i += 1;
            let seg_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i == seg_start {
                return Err("empty escaped identifier in expression".to_string());
            }
            let base = input[seg_start..i].to_string();
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if let Some((after_bracket, next_i, next_seg_start)) =
                scan_hier_bracket_index(input, j, sep, allow_colon_sep)
            {
                segments.push(IdentSeg {
                    name: base,
                    is_esc: true,
                    index: Some(input[j..after_bracket].to_string()),
                });
                i = next_i;
                at_seg_start = next_seg_start;
                continue;
            }
            segments.push(IdentSeg {
                name: base,
                is_esc: true,
                index: None,
            });
            if j + 1 < bytes.len()
                && is_sep_byte(bytes[j])
                && (bytes[j + 1] == b'\\' || bytes[j + 1] == b'.' || is_ident_start(bytes[j + 1]))
            {
                i = j + 1;
                at_seg_start = true;
                continue;
            }
            if j + 1 < bytes.len()
                && bytes[j] == b'.'
                && (bytes[j + 1] == b'\\' || is_ident_start(bytes[j + 1]))
            {
                i = j + 1;
                at_seg_start = false;
                continue;
            }
            break;
        } else if i < bytes.len() && is_ident_start(bytes[i]) {
            let seg_start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            let base = input[seg_start..i].to_string();
            if let Some((after_bracket, next_i, next_seg_start)) =
                scan_hier_bracket_index(input, i, sep, allow_colon_sep)
            {
                segments.push(IdentSeg {
                    name: base,
                    is_esc: false,
                    index: Some(input[i..after_bracket].to_string()),
                });
                i = next_i;
                at_seg_start = next_seg_start;
                continue;
            }
            segments.push(IdentSeg {
                name: base,
                is_esc: false,
                index: None,
            });
        } else {
            let preview: String = input[start..].chars().take(16).collect();
            return Err(format!(
                "unexpected hierarchy separator in identifier near `{preview}`"
            ));
        }
        if i < bytes.len()
            && (bytes[i] == b'/'
                || (sep == ':'
                    && allow_colon_sep
                    && bytes[i] == b':'
                    && bytes
                        .get(i + 1)
                        .is_some_and(|&c| c == b'\\' || c == b'.' || is_ident_start(c))))
        {
            i += 1;
            at_seg_start = true;
            continue;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            if bytes
                .get(i + 1)
                .is_some_and(|&c| c == b'\\' || is_ident_start(c))
            {
                i += 1;
                at_seg_start = false;
                continue;
            }
            let preview: String = input[start..].chars().take(16).collect();
            return Err(format!(
                "unexpected hierarchy separator in identifier near `{preview}`"
            ));
        }
        break;
    }
    if segments.is_empty() {
        return Err("empty identifier in expression".to_string());
    }
    Ok((IdentToken { is_abs, segments }, i))
}

pub(crate) fn tokenize_expr_with_spans(
    input: &str,
    sep: char,
) -> Result<Vec<(ExprTok, Range<usize>)>, String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let mut bracket_depth = 0usize;
    let mut ternary_stack = vec![0usize];
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'{' {
            let tok_start = i;
            let start = i + 1;
            let mut depth = 1usize;
            i += 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                }
                i += 1;
            }
            if depth != 0 {
                return Err("unclosed `{` in expression".to_string());
            }
            let end = i - 1;
            out.push((
                ExprTok::BraceGroup(input[start..end].to_string()),
                tok_start..i,
            ));
            continue;
        }
        let rest = &input[i..];
        if rest.starts_with("===") || rest.starts_with("!==") {
            let tok = if rest.starts_with('=') {
                ExprTok::EqEq
            } else {
                ExprTok::BangEq
            };
            out.push((tok, i..i + 3));
            i += 3;
            continue;
        }
        let two_char = if rest.starts_with("&&") {
            Some(ExprTok::AndAnd)
        } else if rest.starts_with("||") {
            Some(ExprTok::OrOr)
        } else if rest.starts_with("==") {
            Some(ExprTok::EqEq)
        } else if rest.starts_with("!=") {
            Some(ExprTok::BangEq)
        } else if rest.starts_with("<=") {
            Some(ExprTok::Le)
        } else if rest.starts_with(">=") {
            Some(ExprTok::Ge)
        } else if rest.starts_with("<<") {
            Some(ExprTok::Shl)
        } else if rest.starts_with(">>") {
            Some(ExprTok::Shr)
        } else if rest.starts_with("~^") || rest.starts_with("^~") {
            Some(ExprTok::Xnor)
        } else if rest.starts_with("~&") {
            Some(ExprTok::Nand)
        } else if rest.starts_with("~|") {
            Some(ExprTok::Nor)
        } else {
            None
        };
        if let Some(tok) = two_char {
            out.push((tok, i..i + 2));
            i += 2;
            continue;
        }

        let ternary_depth = *ternary_stack.last().unwrap_or(&0);
        match b {
            b'(' => {
                ternary_stack.push(0);
                out.push((ExprTok::LParen, i..i + 1));
                i += 1;
            }
            b')' => {
                if ternary_stack.len() > 1 {
                    ternary_stack.pop();
                }
                out.push((ExprTok::RParen, i..i + 1));
                i += 1;
            }
            b'[' => {
                bracket_depth += 1;
                out.push((ExprTok::LBracket, i..i + 1));
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                out.push((ExprTok::RBracket, i..i + 1));
                i += 1;
            }
            b':' => {
                let prev_expects_operand = matches!(
                    out.last().map(|(t, _)| t),
                    None | Some(
                        ExprTok::Question
                            | ExprTok::Colon
                            | ExprTok::LParen
                            | ExprTok::Bang
                            | ExprTok::Tilde
                            | ExprTok::And
                            | ExprTok::Or
                            | ExprTok::Xor
                            | ExprTok::Xnor
                            | ExprTok::Nand
                            | ExprTok::Nor
                            | ExprTok::AndAnd
                            | ExprTok::OrOr
                            | ExprTok::EqEq
                            | ExprTok::BangEq
                            | ExprTok::Lt
                            | ExprTok::Le
                            | ExprTok::Gt
                            | ExprTok::Ge
                            | ExprTok::Shl
                            | ExprTok::Shr
                    )
                );
                if sep == ':'
                    && bracket_depth == 0
                    && (ternary_depth == 0
                        || (prev_expects_operand && has_following_colon_for_ternary(input, i)))
                    && bytes
                        .get(i + 1)
                        .is_some_and(|&c| c == b'\\' || c == b'.' || is_ident_start(c))
                {
                    let (tok, next_i) = scan_ident_token_with_sep(input, i, sep, true)?;
                    out.push((ExprTok::Ident(tok), i..next_i));
                    i = next_i;
                } else {
                    if bracket_depth == 0 {
                        if let Some(top) = ternary_stack.last_mut() {
                            *top = top.saturating_sub(1);
                        }
                    }
                    out.push((ExprTok::Colon, i..i + 1));
                    i += 1;
                }
            }
            b'?' => {
                if bracket_depth == 0 {
                    if let Some(top) = ternary_stack.last_mut() {
                        *top += 1;
                    }
                }
                out.push((ExprTok::Question, i..i + 1));
                i += 1;
            }
            b'!' | b'~' | b'&' | b'|' | b'^' | b'<' | b'>' => {
                let tok = match b {
                    b'!' => ExprTok::Bang,
                    b'~' => ExprTok::Tilde,
                    b'&' => ExprTok::And,
                    b'|' => ExprTok::Or,
                    b'^' => ExprTok::Xor,
                    b'<' => ExprTok::Lt,
                    b'>' => ExprTok::Gt,
                    _ => unreachable!(),
                };
                out.push((tok, i..i + 1));
                i += 1;
            }
            b'\'' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'\'' || bytes[i] == b'_')
                {
                    i += 1;
                }
                out.push((ExprTok::Number(input[start..i].to_string()), start..i));
            }
            b'\\' | b'/' | b'.' => {
                let allow_colon_sep = sep == ':'
                    && bracket_depth == 0
                    && (ternary_depth == 0 || has_following_colon_for_ternary(input, i));
                let (tok, next_i) = scan_ident_token_with_sep(input, i, sep, allow_colon_sep)?;
                out.push((ExprTok::Ident(tok), i..next_i));
                i = next_i;
            }
            c if is_ident_start(c) => {
                let allow_colon_sep = sep == ':'
                    && bracket_depth == 0
                    && (ternary_depth == 0 || has_following_colon_for_ternary(input, i));
                let (tok, next_i) = scan_ident_token_with_sep(input, i, sep, allow_colon_sep)?;
                out.push((ExprTok::Ident(tok), i..next_i));
                i = next_i;
            }
            _ => {
                let bad = input[i..].chars().next().unwrap_or('?');
                return Err(format!("unexpected character `{bad}` in expression"));
            }
        }
    }
    Ok(out)
}

pub(crate) fn tokenize_expr(input: &str) -> Result<Vec<ExprTok>, String> {
    tokenize_expr_with_spans(input, '/').map(|v| v.into_iter().map(|(tok, _)| tok).collect())
}

pub fn qualify_logic_expr_to_root(expr: &str, scope: &str) -> String {
    qualify_logic_expr_impl(expr, scope, '/', true)
}

pub fn qualify_logic_expr_in_scope(expr: &str, scope: &str, sep: char) -> String {
    qualify_logic_expr_impl(expr, scope, sep, false)
}

pub(crate) fn normalize_logic_expr_sep(expr: &str, sep: char) -> String {
    let Ok(tokens) = tokenize_expr_with_spans(expr, sep) else {
        return expr.to_string();
    };
    let mut out = String::with_capacity(expr.len() + 8);
    let mut cursor = 0usize;
    for (tok, span) in tokens {
        if span.start > cursor {
            out.push_str(&expr[cursor..span.start]);
        }
        match tok {
            ExprTok::Ident(ident) if ident.is_bool_literal().is_none() => {
                if ident.is_abs {
                    out.push('/');
                }
                for (idx, seg) in ident.segments.iter().enumerate() {
                    if idx > 0 {
                        out.push('/');
                    }
                    seg.push_formatted(&mut out, idx + 1 < ident.segments.len());
                }
            }
            _ => out.push_str(&expr[span.start..span.end]),
        }
        cursor = span.end;
    }
    if cursor < expr.len() {
        out.push_str(&expr[cursor..]);
    }
    out
}

fn parse_norm_scope_segment(s: &str) -> IdentSeg {
    if let Some(rest) = s.strip_prefix('\\') {
        if let Some(ws_pos) = rest.find(|c: char| c.is_ascii_whitespace()) {
            let name = rest[..ws_pos].to_string();
            let tail = rest[ws_pos..].trim();
            IdentSeg {
                name,
                is_esc: true,
                index: (!tail.is_empty()).then(|| tail.to_string()),
            }
        } else {
            IdentSeg {
                name: rest.to_string(),
                is_esc: true,
                index: None,
            }
        }
    } else {
        IdentSeg {
            name: s.to_string(),
            is_esc: false,
            index: None,
        }
    }
}

pub(crate) fn resolve_ident_segments(tok: &IdentToken, norm_scope: &str) -> Vec<IdentSeg> {
    resolve_ident_segments_with_escape(tok, norm_scope).0
}

pub(crate) fn resolve_ident_segments_with_escape(
    tok: &IdentToken,
    norm_scope: &str,
) -> (Vec<IdentSeg>, bool) {
    let mut resolved: Vec<IdentSeg> = if !tok.is_abs && !norm_scope.is_empty() {
        split_hier_segments(norm_scope)
            .into_iter()
            .map(parse_norm_scope_segment)
            .collect()
    } else {
        Vec::new()
    };
    let mut escaped_to_root = false;
    for seg in &tok.segments {
        if !seg.is_esc && seg.index.is_none() && seg.name == "." {
            continue;
        }
        if !seg.is_esc && seg.index.is_none() && seg.name == ".." {
            resolved.pop();
            if resolved.is_empty() && !norm_scope.is_empty() {
                escaped_to_root = true;
            }
            continue;
        }
        resolved.push(seg.clone());
    }
    (resolved, escaped_to_root)
}

fn qualify_logic_expr_impl(
    expr: &str,
    scope: &str,
    sep: char,
    force_leading_slash: bool,
) -> String {
    let Ok(tokens) = tokenize_expr_with_spans(expr, sep) else {
        return expr.to_string();
    };
    let norm_scope = normalize_scope(scope, sep);
    let mut out = String::with_capacity(expr.len() + norm_scope.len() + 8);
    let mut cursor = 0usize;
    for (tok, span) in tokens {
        if span.start > cursor {
            out.push_str(&expr[cursor..span.start]);
        }
        match tok {
            ExprTok::Ident(ident) if ident.is_bool_literal().is_none() => {
                let (resolved, escaped_to_root) =
                    resolve_ident_segments_with_escape(&ident, &norm_scope);
                if force_leading_slash || ident.is_abs || escaped_to_root {
                    out.push('/');
                }
                for (idx, seg) in resolved.iter().enumerate() {
                    if idx > 0 {
                        out.push('/');
                    }
                    seg.push_formatted(&mut out, idx + 1 < resolved.len());
                }
            }
            _ => out.push_str(&expr[span.start..span.end]),
        }
        cursor = span.end;
    }
    if cursor < expr.len() {
        out.push_str(&expr[cursor..]);
    }
    out
}
