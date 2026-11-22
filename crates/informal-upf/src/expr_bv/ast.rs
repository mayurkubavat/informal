//! Boolean / bit-vector AST and recursive-descent parser for `-logic_expr`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ir::normalize_scope;

use super::lexer::{resolve_ident_segments, tokenize_expr, ExprTok};

/// Maximum supported SV literal / signal bit width in `-logic_expr`.
pub const MAX_LITERAL_WIDTH: u32 = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
}

/// Pure-data boolean / bit-vector AST exported for downstream formal LPV (`pa_model.rs`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogicConstraintAst {
    LitBool(bool),
    BitConst {
        width: u32,
        bits: Vec<bool>,
    },
    /// Unsized fill literal (`'0` / `'1`) that expands to all-0s or all-1s across the context width.
    FillBit(bool),
    Signal {
        name: String,
        msb: Option<u32>,
        lsb: Option<u32>,
    },
    Not(Box<LogicConstraintAst>),
    BitNot(Box<LogicConstraintAst>),
    RedAnd(Box<LogicConstraintAst>),
    RedOr(Box<LogicConstraintAst>),
    RedXor(Box<LogicConstraintAst>),
    And(Vec<LogicConstraintAst>),
    Or(Vec<LogicConstraintAst>),
    BitAnd(Box<LogicConstraintAst>, Box<LogicConstraintAst>),
    BitOr(Box<LogicConstraintAst>, Box<LogicConstraintAst>),
    BitXor(Box<LogicConstraintAst>, Box<LogicConstraintAst>),
    Eq(Box<LogicConstraintAst>, Box<LogicConstraintAst>),
    Neq(Box<LogicConstraintAst>, Box<LogicConstraintAst>),
    Cmp {
        op: CmpOp,
        lhs: Box<LogicConstraintAst>,
        rhs: Box<LogicConstraintAst>,
    },
    Shl(Box<LogicConstraintAst>, u32),
    Shr(Box<LogicConstraintAst>, u32),
    Ite {
        cond: Box<LogicConstraintAst>,
        then_br: Box<LogicConstraintAst>,
        else_br: Box<LogicConstraintAst>,
    },
}

pub fn collect_logic_expr_signals(ast: &LogicConstraintAst) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_signals_into(ast, &mut out);
    out
}

/// Parses `expr` as a logic expression and returns all referenced signal strings,
/// preserving leading `/` root anchors and optional trailing `[msb]` / `[msb:lsb]` slices.
pub fn collect_logic_expr_signal_refs(expr: &str) -> Result<Vec<String>, String> {
    let _ = parse_logic_expr(expr)?;
    let tokens = tokenize_expr(expr)?;
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        if let ExprTok::Ident(tok) = &tokens[i] {
            if tok.is_bool_literal().is_none() {
                let mut s = String::new();
                if tok.is_abs {
                    s.push('/');
                }
                for (idx, seg) in tok.segments.iter().enumerate() {
                    if idx > 0 {
                        s.push('/');
                    }
                    seg.push_formatted(&mut s, idx + 1 < tok.segments.len());
                }
                let leaf_esc_no_idx = tok
                    .segments
                    .last()
                    .is_some_and(|seg| seg.is_esc && seg.index.is_none());
                if matches!(tokens.get(i + 1), Some(ExprTok::LBracket)) {
                    if let Some(ExprTok::Number(hi)) = tokens.get(i + 2) {
                        if matches!(tokens.get(i + 3), Some(ExprTok::RBracket)) {
                            if leaf_esc_no_idx {
                                s.push(' ');
                            }
                            s.push_str(&format!("[{hi}]"));
                            i += 3;
                        } else if matches!(tokens.get(i + 3), Some(ExprTok::Colon)) {
                            if let (Some(ExprTok::Number(lo)), Some(ExprTok::RBracket)) =
                                (tokens.get(i + 4), tokens.get(i + 5))
                            {
                                if leaf_esc_no_idx {
                                    s.push(' ');
                                }
                                s.push_str(&format!("[{hi}:{lo}]"));
                                i += 5;
                            }
                        }
                    }
                }
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        i += 1;
    }
    Ok(out)
}

pub(crate) fn collect_signals_into(ast: &LogicConstraintAst, out: &mut BTreeSet<String>) {
    match ast {
        LogicConstraintAst::LitBool(_)
        | LogicConstraintAst::BitConst { .. }
        | LogicConstraintAst::FillBit(_) => {}
        LogicConstraintAst::Signal { name, .. } => {
            out.insert(name.clone());
        }
        LogicConstraintAst::Not(x)
        | LogicConstraintAst::BitNot(x)
        | LogicConstraintAst::RedAnd(x)
        | LogicConstraintAst::RedOr(x)
        | LogicConstraintAst::RedXor(x)
        | LogicConstraintAst::Shl(x, _)
        | LogicConstraintAst::Shr(x, _) => collect_signals_into(x, out),
        LogicConstraintAst::And(items) | LogicConstraintAst::Or(items) => {
            for it in items {
                collect_signals_into(it, out);
            }
        }
        LogicConstraintAst::BitAnd(a, b)
        | LogicConstraintAst::BitOr(a, b)
        | LogicConstraintAst::BitXor(a, b)
        | LogicConstraintAst::Eq(a, b)
        | LogicConstraintAst::Neq(a, b)
        | LogicConstraintAst::Cmp { lhs: a, rhs: b, .. } => {
            collect_signals_into(a, out);
            collect_signals_into(b, out);
        }
        LogicConstraintAst::Ite {
            cond,
            then_br,
            else_br,
        } => {
            collect_signals_into(cond, out);
            collect_signals_into(then_br, out);
            collect_signals_into(else_br, out);
        }
    }
}

pub fn parse_logic_expr(expr: &str) -> Result<LogicConstraintAst, String> {
    parse_logic_expr_in_scope(expr, "")
}

pub fn parse_logic_expr_in_scope(expr: &str, scope: &str) -> Result<LogicConstraintAst, String> {
    let tokens = tokenize_expr(expr)?;
    if tokens.is_empty() {
        return Err("empty logic expression".to_string());
    }
    let norm_scope = normalize_scope(scope, '/');
    let mut pos = 0;
    let ast = parse_ternary(&tokens, &mut pos, &norm_scope)?;
    if pos != tokens.len() {
        return Err(format!(
            "unexpected trailing token {:?} in logic expression `{expr}`",
            tokens[pos]
        ));
    }
    Ok(ast)
}

fn parse_ternary(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let cond = parse_log_or(tokens, pos, scope)?;
    if matches!(tokens.get(*pos), Some(ExprTok::Question)) {
        *pos += 1;
        let t = parse_ternary(tokens, pos, scope)?;
        if !matches!(tokens.get(*pos), Some(ExprTok::Colon)) {
            return Err("expected `:` in ternary expression".to_string());
        }
        *pos += 1;
        let e = parse_ternary(tokens, pos, scope)?;
        Ok(LogicConstraintAst::Ite {
            cond: Box::new(cond),
            then_br: Box::new(t),
            else_br: Box::new(e),
        })
    } else {
        Ok(cond)
    }
}

fn parse_log_or(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut items = vec![parse_log_and(tokens, pos, scope)?];
    while matches!(tokens.get(*pos), Some(ExprTok::OrOr)) {
        *pos += 1;
        items.push(parse_log_and(tokens, pos, scope)?);
    }
    if items.len() == 1 {
        Ok(items.pop().unwrap())
    } else {
        Ok(LogicConstraintAst::Or(items))
    }
}

fn parse_log_and(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut items = vec![parse_bit_or(tokens, pos, scope)?];
    while matches!(tokens.get(*pos), Some(ExprTok::AndAnd)) {
        *pos += 1;
        items.push(parse_bit_or(tokens, pos, scope)?);
    }
    if items.len() == 1 {
        Ok(items.pop().unwrap())
    } else {
        Ok(LogicConstraintAst::And(items))
    }
}

fn parse_bit_or(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut lhs = parse_bit_xor(tokens, pos, scope)?;
    while matches!(tokens.get(*pos), Some(ExprTok::Or)) {
        *pos += 1;
        let rhs = parse_bit_xor(tokens, pos, scope)?;
        lhs = LogicConstraintAst::BitOr(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_bit_xor(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut lhs = parse_bit_and(tokens, pos, scope)?;
    while matches!(tokens.get(*pos), Some(ExprTok::Xor | ExprTok::Xnor)) {
        let is_xnor = matches!(tokens.get(*pos), Some(ExprTok::Xnor));
        *pos += 1;
        let rhs = parse_bit_and(tokens, pos, scope)?;
        let x = LogicConstraintAst::BitXor(Box::new(lhs), Box::new(rhs));
        lhs = if is_xnor {
            LogicConstraintAst::BitNot(Box::new(x))
        } else {
            x
        };
    }
    Ok(lhs)
}

fn parse_bit_and(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut lhs = parse_eq(tokens, pos, scope)?;
    while matches!(tokens.get(*pos), Some(ExprTok::And)) {
        *pos += 1;
        let rhs = parse_eq(tokens, pos, scope)?;
        lhs = LogicConstraintAst::BitAnd(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_eq(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut lhs = parse_rel(tokens, pos, scope)?;
    while matches!(tokens.get(*pos), Some(ExprTok::EqEq | ExprTok::BangEq)) {
        let is_eq = matches!(tokens.get(*pos), Some(ExprTok::EqEq));
        *pos += 1;
        let rhs = parse_rel(tokens, pos, scope)?;
        lhs = if is_eq {
            LogicConstraintAst::Eq(Box::new(lhs), Box::new(rhs))
        } else {
            LogicConstraintAst::Neq(Box::new(lhs), Box::new(rhs))
        };
    }
    Ok(lhs)
}

fn parse_rel(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut lhs = parse_shift(tokens, pos, scope)?;
    while let Some(op) = match tokens.get(*pos) {
        Some(ExprTok::Lt) => Some(CmpOp::Lt),
        Some(ExprTok::Le) => Some(CmpOp::Le),
        Some(ExprTok::Gt) => Some(CmpOp::Gt),
        Some(ExprTok::Ge) => Some(CmpOp::Ge),
        _ => None,
    } {
        *pos += 1;
        let rhs = parse_shift(tokens, pos, scope)?;
        lhs = LogicConstraintAst::Cmp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_shift(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    let mut lhs = parse_unary(tokens, pos, scope)?;
    while matches!(tokens.get(*pos), Some(ExprTok::Shl | ExprTok::Shr)) {
        let is_shl = matches!(tokens.get(*pos), Some(ExprTok::Shl));
        *pos += 1;
        let rhs = parse_unary(tokens, pos, scope)?;
        let amt = match rhs {
            LogicConstraintAst::BitConst { bits, .. } => {
                let mut val = 0u32;
                for (idx, &b) in bits.iter().enumerate() {
                    if b {
                        if idx >= 32 {
                            val = u32::MAX;
                            break;
                        }
                        val |= 1u32 << idx;
                    }
                }
                val
            }
            LogicConstraintAst::FillBit(b) => u32::from(b),
            _ => return Err("non-constant shift amount in -logic_expr is unsupported".to_string()),
        };
        lhs = if is_shl {
            LogicConstraintAst::Shl(Box::new(lhs), amt)
        } else {
            LogicConstraintAst::Shr(Box::new(lhs), amt)
        };
    }
    Ok(lhs)
}

fn parse_unary(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    match tokens.get(*pos) {
        Some(ExprTok::Bang) => {
            *pos += 1;
            Ok(LogicConstraintAst::Not(Box::new(parse_unary(
                tokens, pos, scope,
            )?)))
        }
        Some(ExprTok::Tilde) => {
            *pos += 1;
            Ok(LogicConstraintAst::BitNot(Box::new(parse_unary(
                tokens, pos, scope,
            )?)))
        }
        Some(ExprTok::And) => {
            *pos += 1;
            Ok(LogicConstraintAst::RedAnd(Box::new(parse_unary(
                tokens, pos, scope,
            )?)))
        }
        Some(ExprTok::Or) => {
            *pos += 1;
            Ok(LogicConstraintAst::RedOr(Box::new(parse_unary(
                tokens, pos, scope,
            )?)))
        }
        Some(ExprTok::Xor) => {
            *pos += 1;
            Ok(LogicConstraintAst::RedXor(Box::new(parse_unary(
                tokens, pos, scope,
            )?)))
        }
        Some(ExprTok::Xnor) => {
            *pos += 1;
            Ok(LogicConstraintAst::Not(Box::new(
                LogicConstraintAst::RedXor(Box::new(parse_unary(tokens, pos, scope)?)),
            )))
        }
        Some(ExprTok::Nand) => {
            *pos += 1;
            Ok(LogicConstraintAst::Not(Box::new(
                LogicConstraintAst::RedAnd(Box::new(parse_unary(tokens, pos, scope)?)),
            )))
        }
        Some(ExprTok::Nor) => {
            *pos += 1;
            Ok(LogicConstraintAst::Not(Box::new(
                LogicConstraintAst::RedOr(Box::new(parse_unary(tokens, pos, scope)?)),
            )))
        }
        _ => parse_primary(tokens, pos, scope),
    }
}

fn parse_primary(
    tokens: &[ExprTok],
    pos: &mut usize,
    scope: &str,
) -> Result<LogicConstraintAst, String> {
    match tokens.get(*pos) {
        Some(ExprTok::LParen) => {
            *pos += 1;
            let inner = parse_ternary(tokens, pos, scope)?;
            if !matches!(tokens.get(*pos), Some(ExprTok::RParen)) {
                return Err("expected `)` in -logic_expr".to_string());
            }
            *pos += 1;
            Ok(inner)
        }
        Some(ExprTok::Number(s)) => {
            *pos += 1;
            parse_sv_literal(s)
        }
        Some(ExprTok::Ident(tok)) => {
            *pos += 1;
            if let Some(b) = tok.is_bool_literal() {
                return Ok(LogicConstraintAst::LitBool(b));
            }
            let name = resolve_ident_segments(tok, scope)
                .into_iter()
                .map(|seg| seg.as_ast_part())
                .collect::<Vec<_>>()
                .join("/");
            let mut msb = None;
            let mut lsb = None;
            if matches!(tokens.get(*pos), Some(ExprTok::LBracket)) {
                *pos += 1;
                let Some(ExprTok::Number(hi_s)) = tokens.get(*pos) else {
                    return Err("expected bit index inside `[...]`".to_string());
                };
                let hi: u32 = hi_s
                    .parse()
                    .map_err(|_| format!("invalid bit index `{hi_s}`"))?;
                *pos += 1;
                if matches!(tokens.get(*pos), Some(ExprTok::Colon)) {
                    *pos += 1;
                    let Some(ExprTok::Number(lo_s)) = tokens.get(*pos) else {
                        return Err("expected lsb in slice".to_string());
                    };
                    let lo: u32 = lo_s
                        .parse()
                        .map_err(|_| format!("invalid bit index `{lo_s}`"))?;
                    *pos += 1;
                    if hi.abs_diff(lo) >= MAX_LITERAL_WIDTH {
                        return Err(format!(
                            "bit slice `[{hi}:{lo}]` width exceeds maximum supported width {MAX_LITERAL_WIDTH}"
                        ));
                    }
                    msb = Some(hi);
                    lsb = Some(lo);
                } else {
                    msb = Some(hi);
                    lsb = Some(hi);
                }
                if !matches!(tokens.get(*pos), Some(ExprTok::RBracket)) {
                    return Err("expected `]` after bit slice".to_string());
                }
                *pos += 1;
            }
            Ok(LogicConstraintAst::Signal { name, msb, lsb })
        }
        other => Err(format!("unexpected token in -logic_expr: {other:?}")),
    }
}

fn decimal_str_to_lsb_bits(
    digits: &str,
    raw: &str,
    max_bits: Option<usize>,
) -> Result<Vec<bool>, String> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid decimal literal `{raw}`"));
    }
    let mut dec: Vec<u8> = digits.bytes().map(|b| b - b'0').collect();
    let mut bits = Vec::new();
    while dec.iter().any(|&d| d != 0) {
        if let Some(limit) = max_bits {
            if bits.len() >= limit {
                break;
            }
        }
        let mut rem = 0u8;
        for d in &mut dec {
            let cur = rem * 10 + *d;
            *d = cur / 2;
            rem = cur % 2;
        }
        bits.push(rem == 1);
    }
    Ok(bits)
}

fn parse_sv_literal(raw: &str) -> Result<LogicConstraintAst, String> {
    let cleaned = raw.replace('_', "");
    if let Some((w_str, rest)) = cleaned.split_once('\'') {
        let explicit_width = if w_str.is_empty() {
            None
        } else {
            let w: u32 = w_str
                .parse()
                .map_err(|_| format!("invalid literal width `{raw}`"))?;
            if w == 0 || w > MAX_LITERAL_WIDTH {
                return Err(format!(
                    "literal width `{w}` out of supported range 1..={MAX_LITERAL_WIDTH} in `{raw}`"
                ));
            }
            Some(w)
        };
        let (radix, digits) = match rest.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('b') => (2usize, &rest[1..]),
            Some('o') => (8usize, &rest[1..]),
            Some('d') => (10usize, &rest[1..]),
            Some('h') => (16usize, &rest[1..]),
            Some('0') if rest.len() == 1 => {
                if let Some(width) = explicit_width {
                    return Ok(LogicConstraintAst::BitConst {
                        width,
                        bits: vec![false; width as usize],
                    });
                }
                return Ok(LogicConstraintAst::FillBit(false));
            }
            Some('1') if rest.len() == 1 => {
                if let Some(width) = explicit_width {
                    return Ok(LogicConstraintAst::BitConst {
                        width,
                        bits: vec![true; width as usize],
                    });
                }
                return Ok(LogicConstraintAst::FillBit(true));
            }
            Some('x') | Some('z') | Some('?') => {
                return Err(format!(
                    "four-state X/Z literal `{raw}` is not supported in 2-state SAT -logic_expr"
                ))
            }
            _ => return Err(format!("unsupported SV literal `{raw}`")),
        };
        if digits.is_empty() {
            return Err(format!("missing digits in SV literal `{raw}`"));
        }
        if digits
            .chars()
            .any(|c| matches!(c, 'x' | 'X' | 'z' | 'Z' | '?'))
        {
            return Err(format!(
                "four-state X/Z literal `{raw}` is not supported in 2-state SAT -logic_expr"
            ));
        }

        let mut bits = Vec::new();
        let width: u32 = match radix {
            2 | 8 | 16 => {
                let bits_per_digit = match radix {
                    2 => 1usize,
                    8 => 3usize,
                    16 => 4usize,
                    _ => unreachable!(),
                };
                for ch in digits.chars().rev() {
                    let d = ch
                        .to_digit(radix as u32)
                        .ok_or_else(|| format!("invalid SV literal `{raw}`"))?;
                    for k in 0..bits_per_digit {
                        bits.push(((d >> k) & 1) == 1);
                    }
                }
                let natural_w = u32::try_from(bits.len().max(1))
                    .map_err(|_| format!("literal width overflow in `{raw}`"))?;
                let w = explicit_width.unwrap_or(natural_w).min(MAX_LITERAL_WIDTH);
                bits.truncate(w as usize);
                bits.resize(w as usize, false);
                w
            }
            10 => {
                let max_b = Some(explicit_width.unwrap_or(MAX_LITERAL_WIDTH) as usize);
                bits = decimal_str_to_lsb_bits(digits, raw, max_b)?;
                let natural_w = u32::try_from(bits.len().max(1))
                    .map_err(|_| format!("literal width overflow in `{raw}`"))?;
                let w = explicit_width.unwrap_or(natural_w).min(MAX_LITERAL_WIDTH);
                bits.truncate(w as usize);
                bits.resize(w as usize, false);
                w
            }
            _ => unreachable!(),
        };
        Ok(LogicConstraintAst::BitConst { width, bits })
    } else {
        let mut bits = decimal_str_to_lsb_bits(&cleaned, raw, Some(MAX_LITERAL_WIDTH as usize))?;
        if bits.is_empty() {
            bits.push(false);
        }
        let width = u32::try_from(bits.len())
            .map_err(|_| format!("integer literal `{raw}` exceeds maximum supported width"))?;
        Ok(LogicConstraintAst::BitConst { width, bits })
    }
}
