//! Live Formal-Ready linting (`svlang-lint` `SVA-FPV-001..005` + SVA/CDC/FSM checks)
//! with 1-click quick-fixes and RTL Go-to-Driver / Go-to-Load navigation.

use crate::types::Request;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalQuickFix {
    pub file: String,
    /// 0-based UTF-8 byte offset start in `file`.
    pub start: u32,
    /// 0-based UTF-8 byte offset end in `file`.
    pub end: u32,
    /// 1-based line of edit start.
    pub line: u32,
    pub replacement: String,
    pub description: String,
    /// `"safe"` or `"unsafe"` (e.g. `assume` -> `assert` changes proof role).
    pub safety: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalLintFinding {
    pub rule: String,
    pub name: String,
    /// `"error"`, `"warning"`, `"info"`, or `"style"`.
    pub severity: String,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub fixes: Vec<FormalQuickFix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRef {
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// `"declaration"`, `"driver"`, or `"load"`.
    pub role: String,
    /// `"input"`, `"output"`, `"reg"`, `"always_ff"`, `"always_comb"`, `"assign"`, `"property"`, `"expr"`.
    pub kind: String,
    pub scope: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalNav {
    pub signal: String,
    pub leaf: String,
    pub declarations: Vec<SignalRef>,
    pub drivers: Vec<SignalRef>,
    pub loads: Vec<SignalRef>,
}

fn byte_to_line(src: &str, byte_idx: usize) -> u32 {
    let clamped = byte_idx.min(src.len());
    (src.as_bytes()[..clamped]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1) as u32
}

fn line_byte_span(src: &str, line_1based: u32) -> Option<(usize, usize)> {
    if line_1based == 0 {
        return None;
    }
    let mut cur_line = 1u32;
    let mut start = 0usize;
    for (idx, b) in src.bytes().enumerate() {
        if cur_line == line_1based {
            let end = src[idx..].find('\n').map_or(src.len(), |rel| idx + rel);
            return Some((idx, end));
        }
        if b == b'\n' {
            cur_line += 1;
            start = idx + 1;
        }
    }
    if cur_line == line_1based && start <= src.len() {
        Some((start, src.len()))
    } else {
        None
    }
}

/// Synthesize a 1-click quick-fix for `SVA-FPV-001` (`assume` -> `assert`) or
/// `SVA-FPV-003` (insert `disable iff (...)`) when the underlying syntax rule
/// emitted a diagnostic without a byte-level rewrite.
fn synthesize_fpv_fix(
    rule: &str,
    file: &str,
    line: u32,
    src: &str,
    reset_hint: Option<&str>,
) -> Option<FormalQuickFix> {
    let (lstart, lend) = line_byte_span(src, line)?;
    let line_str = &src[lstart..lend];
    match rule {
        "SVA-FPV-001" => {
            // Find standalone keyword `assume` on the diagnostic line.
            let bytes = line_str.as_bytes();
            let kw = b"assume";
            for idx in 0..bytes.len().saturating_sub(kw.len() - 1) {
                if &bytes[idx..idx + kw.len()] == kw {
                    let prev_ok = idx == 0 || !is_ident_char(bytes[idx - 1]);
                    let next_ok =
                        idx + kw.len() == bytes.len() || !is_ident_char(bytes[idx + kw.len()]);
                    if prev_ok && next_ok {
                        return Some(FormalQuickFix {
                            file: file.to_string(),
                            start: (lstart + idx) as u32,
                            end: (lstart + idx + kw.len()) as u32,
                            line,
                            replacement: "assert".to_string(),
                            description:
                                "Convert over-constraining `assume` on output/state to `assert`"
                                    .to_string(),
                            safety: "unsafe".to_string(),
                        });
                    }
                }
            }
            None
        }
        "SVA-FPV-003" => {
            let rst_expr = reset_hint
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("!rst_n");
            // Look for `@(posedge ...)` or `property (` on the line or next line to insert `disable iff (rst_expr) `.
            let window_end = src[lend..]
                .find('\n')
                .map_or(src.len(), |n| (lend + 1 + n).min(src.len()));
            let span_str = &src[lstart..window_end];
            if span_str.contains("disable iff") {
                return None;
            }
            if let Some(at_pos) = span_str.find("@(") {
                // Find matching `)` of `@(...)`
                let mut depth = 0i32;
                for (off, ch) in span_str[at_pos + 1..].char_indices() {
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            let insert_at = lstart + at_pos + 1 + off + 1;
                            return Some(FormalQuickFix {
                                file: file.to_string(),
                                start: insert_at as u32,
                                end: insert_at as u32,
                                line: byte_to_line(src, insert_at),
                                replacement: format!(" disable iff ({rst_expr})"),
                                description: format!("Insert `disable iff ({rst_expr})`"),
                                safety: "safe".to_string(),
                            });
                        }
                    }
                }
            }
            if let Some(prop_pos) = line_str.find("property") {
                let after = prop_pos + "property".len();
                if let Some(open_rel) = line_str[after..].find('(') {
                    let insert_at = lstart + after + open_rel + 1;
                    return Some(FormalQuickFix {
                        file: file.to_string(),
                        start: insert_at as u32,
                        end: insert_at as u32,
                        line,
                        replacement: format!("disable iff ({rst_expr}) "),
                        description: format!("Insert `disable iff ({rst_expr})`"),
                        safety: "safe".to_string(),
                    });
                }
            }
            None
        }
        _ => None,
    }
}

/// Run `svlang-lint` with the `formal-ready` rule pack (`SVA-FPV-001..005`, SVA, CDC, FSM)
/// in-process on `req`'s SystemVerilog files and return structured findings + quick-fixes.
pub fn lint_formal_ready(
    req: &Request,
    reset_hint: Option<&str>,
) -> Result<Vec<FormalLintFinding>, String> {
    if req.files.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["--single-unit".to_string()];
    for inc in &req.incdirs {
        args.push(format!("-I{}", inc.display()));
    }
    for (k, v) in &req.defines {
        match v {
            Some(val) if !val.is_empty() => args.push(format!("-D{k}={val}")),
            _ => args.push(format!("-D{k}")),
        }
    }
    for f in &req.files {
        args.push(f.display().to_string());
    }
    let (trees, parse_ms) = svlang::semantic::parse_args(&args).map_err(|e| e.to_string())?;
    let (_design, (files, found)) = svlang_lint::lint_built(
        || svlang_model::design_of(&trees, parse_ms),
        &trees,
        svlang_lint::is_formal_ready_rule,
    );

    let mut source_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut read_cached = |p: &str| -> Option<String> {
        if p.is_empty() {
            return None;
        }
        if let Some(s) = source_cache.get(p) {
            return Some(s.clone());
        }
        let text = std::fs::read_to_string(p).ok()?;
        source_cache.insert(p.to_string(), text.clone());
        Some(text)
    };

    let mut out = Vec::with_capacity(found.len());
    for f in found {
        let (file, line, col) = f.position_in(&files);
        let src_opt = read_cached(file);
        let mut fixes: Vec<FormalQuickFix> = f
            .fixes
            .iter()
            .map(|fx| {
                let fx_line = read_cached(&fx.path)
                    .as_deref()
                    .map_or(line, |s| byte_to_line(s, fx.start as usize));
                FormalQuickFix {
                    file: fx.path.clone(),
                    start: fx.start,
                    end: fx.end,
                    line: fx_line,
                    replacement: fx.text.clone(),
                    description: fx.description.clone(),
                    safety: fx.safety.as_str().to_string(),
                }
            })
            .collect();
        if fixes.is_empty() {
            if let Some(ref src) = src_opt {
                if let Some(synth) = synthesize_fpv_fix(f.rule, file, line, src, reset_hint) {
                    fixes.push(synth);
                }
            }
        }
        out.push(FormalLintFinding {
            rule: f.rule.to_string(),
            name: f.name.to_string(),
            severity: f.severity.as_str().to_ascii_lowercase(),
            message: f.message,
            file: file.to_string(),
            line,
            column: col,
            fixes,
        });
    }
    Ok(out)
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn find_word_col(line: &str, leaf: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let target = leaf.as_bytes();
    if target.is_empty() || bytes.len() < target.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut i = 0;
    while i + target.len() <= bytes.len() {
        if &bytes[i..i + target.len()] == target {
            let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let next_ok =
                i + target.len() == bytes.len() || !is_ident_char(bytes[i + target.len()]);
            if prev_ok && next_ok {
                hits.push(i);
                i += target.len();
                continue;
            }
        }
        i += 1;
    }
    hits
}

fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Resolve declarations, drivers, and loads of `signal` (leaf or hierarchical path)
/// across the design's RTL files so CodeMirror 6 and AI tools can jump to drivers/loads.
pub fn navigate_signal(req: &Request, signal: &str) -> Result<SignalNav, String> {
    let trimmed = signal.trim();
    if trimmed.is_empty() {
        return Err("signal name is empty".to_string());
    }
    let leaf = trimmed
        .rsplit('.')
        .next()
        .unwrap_or(trimmed)
        .split('[')
        .next()
        .unwrap_or(trimmed)
        .trim();
    if leaf.is_empty() {
        return Err(format!("invalid signal '{signal}'"));
    }

    let scope_filter: Option<&str> = trimmed.rsplit_once('.').map(|(sc, _)| sc);
    let mut declarations = Vec::new();
    let mut drivers = Vec::new();
    let mut loads = Vec::new();

    for path in &req.files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let file_str = path.display().to_string();
        let mut cur_module = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("top")
            .to_string();
        let mut cur_proc: Option<&'static str> = None;
        let mut in_block_comment = false;

        for (idx, raw_line) in src.lines().enumerate() {
            let lno = (idx + 1) as u32;
            let mut clean = raw_line;
            if in_block_comment {
                if let Some(end) = clean.find("*/") {
                    in_block_comment = false;
                    clean = &clean[end + 2..];
                } else {
                    continue;
                }
            }
            if let Some(start) = clean.find("/*") {
                if let Some(end_rel) = clean[start + 2..].find("*/") {
                    let after = &clean[start + 2 + end_rel + 2..];
                    clean = if start > 0 { &clean[..start] } else { after };
                } else {
                    in_block_comment = true;
                    clean = &clean[..start];
                }
            }
            let code = strip_line_comment(clean).trim();
            if code.is_empty() {
                continue;
            }

            if let Some(rest) = code.strip_prefix("module ") {
                let mname: String = rest
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !mname.is_empty() {
                    cur_module = mname;
                }
            } else if code.starts_with("endmodule") {
                cur_proc = None;
            }
            if code.contains("always_ff") {
                cur_proc = Some("always_ff");
            } else if code.contains("always_comb") || code.contains("always @*") {
                cur_proc = Some("always_comb");
            } else if code.starts_with("assign ") {
                cur_proc = None;
            }

            // If caller specified `mod_or_inst.sig` and module neither matches nor is top, keep scanning all modules
            // because `u_fifo.count` maps to module `fifo`.
            let _ = scope_filter;

            let cols = find_word_col(clean, leaf);
            if cols.is_empty() {
                continue;
            }
            let col = (cols[0] + 1) as u32;
            let snippet = raw_line.trim().to_string();

            // Classify line: declaration vs driver vs load.
            let is_decl = ["input", "output", "inout", "logic", "reg", "wire", "bit"]
                .iter()
                .any(|kw| {
                    code.starts_with(kw)
                        && code[kw.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_whitespace() || c == '[')
                });
            if is_decl {
                let kind = if code.starts_with("input") {
                    "input"
                } else if code.starts_with("output") {
                    "output"
                } else if code.starts_with("inout") {
                    "inout"
                } else {
                    "logic"
                };
                declarations.push(SignalRef {
                    file: file_str.clone(),
                    line: lno,
                    column: col,
                    role: "declaration".to_string(),
                    kind: kind.to_string(),
                    scope: cur_module.clone(),
                    snippet: snippet.clone(),
                });
                // `wire x = expr;` or `logic x = expr;` is also a continuous driver when `=` follows `leaf`.
                if let Some(eq_pos) = code.find('=') {
                    if !code[eq_pos..].starts_with("==") && code[..eq_pos].contains(leaf) {
                        drivers.push(SignalRef {
                            file: file_str.clone(),
                            line: lno,
                            column: col,
                            role: "driver".to_string(),
                            kind: "assign".to_string(),
                            scope: cur_module.clone(),
                            snippet,
                        });
                    }
                }
                continue;
            }

            let is_prop = ["assert", "assume", "cover", "property"]
                .iter()
                .any(|kw| !find_word_col(code, kw).is_empty());
            if is_prop {
                loads.push(SignalRef {
                    file: file_str.clone(),
                    line: lno,
                    column: col,
                    role: "load".to_string(),
                    kind: "property".to_string(),
                    scope: cur_module.clone(),
                    snippet,
                });
                continue;
            }

            // Check nonblocking `<=` or blocking/continuous `=` assignment LHS vs RHS.
            let assign_split = find_assignment_operator(code);
            if let Some((lhs, rhs, is_nb)) = assign_split {
                let on_lhs = !find_word_col(lhs, leaf).is_empty();
                let on_rhs = !find_word_col(rhs, leaf).is_empty();
                if on_lhs {
                    let drv_kind = if code.starts_with("assign ") {
                        "assign"
                    } else if is_nb {
                        cur_proc.unwrap_or("always_ff")
                    } else {
                        cur_proc.unwrap_or("always_comb")
                    };
                    drivers.push(SignalRef {
                        file: file_str.clone(),
                        line: lno,
                        column: col,
                        role: "driver".to_string(),
                        kind: drv_kind.to_string(),
                        scope: cur_module.clone(),
                        snippet: snippet.clone(),
                    });
                }
                if on_rhs {
                    loads.push(SignalRef {
                        file: file_str.clone(),
                        line: lno,
                        column: col,
                        role: "load".to_string(),
                        kind: "expr".to_string(),
                        scope: cur_module.clone(),
                        snippet: snippet.clone(),
                    });
                }
                if on_lhs || on_rhs {
                    continue;
                }
            }

            // Port connection `.port(sig)` or control condition `if (sig)` / `case (sig)`
            loads.push(SignalRef {
                file: file_str.clone(),
                line: lno,
                column: col,
                role: "load".to_string(),
                kind: "expr".to_string(),
                scope: cur_module.clone(),
                snippet,
            });
        }
    }

    Ok(SignalNav {
        signal: trimmed.to_string(),
        leaf: leaf.to_string(),
        declarations,
        drivers,
        loads,
    })
}

/// Split `lhs <= rhs` or `lhs = rhs` while ignoring `==`, `!=`, `<=` inside `if (...)`, `>=`, `===`.
fn find_assignment_operator(code: &str) -> Option<(&str, &str, bool)> {
    // Strip leading `if (...)` / `else if (...)` condition prefix on single-line statements so
    // `if (a <= b) q <= d;` splits at `q <= d` instead of `a <= b`.
    let mut base_off = 0usize;
    let trimmed = code.trim_start();
    for kw in ["else if", "if"] {
        if let Some(rest) = trimmed.strip_prefix(kw) {
            if let Some(open) = rest.find('(') {
                let mut depth = 0i32;
                for (off, ch) in rest[open..].char_indices() {
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            let consumed = (code.len() - trimmed.len()) + kw.len() + open + off + 1;
                            base_off = consumed.min(code.len());
                            break;
                        }
                    }
                }
            }
            break;
        }
    }
    let tail = &code[base_off..];
    let bytes = tail.as_bytes();
    let mut paren = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => paren += 1,
            b')' => paren = (paren - 1).max(0),
            b'<' if paren == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                return Some((&tail[..i], &tail[i + 2..], true));
            }
            b'=' if paren == 0 => {
                let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if !matches!(prev, b'=' | b'!' | b'<' | b'>') && next != b'=' {
                    return Some((&tail[..i], &tail[i + 1..], false));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_formal_ready_surfaces_sv_fpv_rules_and_quick_fixes() {
        let dir = std::env::temp_dir().join(format!("informal-assist-lint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sv = dir.join("dut.sv");
        std::fs::write(
            &sv,
            "\
module dut (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    output logic [3:0] count
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) count <= 4'd0;
        else if (en) count <= count + 4'd1;
    end
    assume_out: assume property (@(posedge clk) count < 4'd12);
    assert_nodis: assert property (@(posedge clk) count <= 4'd15);
endmodule
",
        )
        .unwrap();
        let req = Request {
            files: vec![sv.clone()],
            top: Some("dut".to_string()),
            ..Default::default()
        };
        let diags = lint_formal_ready(&req, Some("!rst_n")).unwrap();
        let rules: Vec<&str> = diags.iter().map(|d| d.rule.as_str()).collect();
        assert!(
            rules.contains(&"SVA-FPV-001"),
            "expected SVA-FPV-001 in {diags:?}"
        );
        assert!(
            rules.contains(&"SVA-FPV-003"),
            "expected SVA-FPV-003 in {diags:?}"
        );

        let fpv1 = diags.iter().find(|d| d.rule == "SVA-FPV-001").unwrap();
        assert!(
            !fpv1.fixes.is_empty(),
            "SVA-FPV-001 should carry a quick-fix"
        );
        assert!(fpv1.fixes[0].replacement.contains("assert"));

        let fpv3 = diags.iter().find(|d| d.rule == "SVA-FPV-003").unwrap();
        assert!(
            !fpv3.fixes.is_empty(),
            "SVA-FPV-003 should carry a quick-fix"
        );
        assert!(fpv3.fixes[0].replacement.contains("disable iff (!rst_n)"));

        let nav = navigate_signal(&req, "dut.count").unwrap();
        assert_eq!(nav.declarations.len(), 1);
        assert_eq!(nav.declarations[0].kind, "output");
        assert_eq!(nav.drivers.len(), 2);
        assert!(nav.loads.iter().any(|l| l.kind == "property"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
