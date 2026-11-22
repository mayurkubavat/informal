//! Formal Connectivity App (`check_conn`).
//!
//! Supports both JasperGold / OpenTitan connectivity spreadsheets (`CONNECTION`,
//! column-wise OR-of-ANDs `CONDITION` truth-table matrices, and optional
//! `LATENCY` rows) and compact 3–5 column CSVs (`name, src, dst [, cond [, delay]]`).
//! Loaded connections populate the `Connectivity` task (or `-task <name>`) with
//! SVA assertions located at their CSV source line, ready for
//! `prove -task Connectivity` or `check_conn -prove`.

use crate::session::{Kind, Session, SourceLoc, EMBEDDED};
use std::path::Path;

pub const DEFAULT_CONN_TASK: &str = "Connectivity";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnRule {
    pub name: String,
    pub src: String,
    pub dst: String,
    /// Optional enabling condition (column-wise OR-of-ANDs from `CONDITION` rows,
    /// or column 3 of a simple CSV).
    pub cond: Option<String>,
    /// Minimum and maximum pipeline latency in clock cycles (`(0, 0)` for combinational wire).
    pub latency: (u32, u32),
    /// 1-based CSV line number of the `CONNECTION` row.
    pub line: u32,
}

impl ConnRule {
    /// Builds the SVA property expression for this connectivity rule.
    pub fn to_sva_expr(&self) -> String {
        let (min_d, max_d) = self.latency;
        let eq_at = |d: u32| -> String {
            if d == 0 {
                format!("(({}) == ({}))", self.dst, self.src)
            } else {
                format!("##{d} (({}) == $past({}, {d}))", self.dst, self.src)
            }
        };
        let consequent = if min_d == max_d {
            eq_at(min_d)
        } else {
            let terms: Vec<String> = (min_d..=max_d).map(|d| format!("({})", eq_at(d))).collect();
            format!("({})", terms.join(" or "))
        };
        match (&self.cond, min_d == 0 && max_d == 0) {
            (None, true) => consequent,
            (None, false) => format!("1'b1 |-> {consequent}"),
            (Some(c), _) => format!("({c}) |-> {consequent}"),
        }
    }
}

/// Ensures `task` exists on `s` (inheriting active assumptions from `<embedded>`
/// if newly created).
pub fn ensure_conn_task(s: &mut Session, task: &str) -> Result<String, String> {
    if !s.tasks.iter().any(|t| t.name == task) {
        s.create_task(task)?;
        let emb_assumes: Vec<usize> = (0..s.props.len())
            .filter(|&i| {
                s.props[i].task == EMBEDDED && s.props[i].kind == Kind::Assume && s.props[i].enabled
            })
            .collect();
        let _ = s.copy_into(task, &emb_assumes);
        let _ = s.copy_stopats(EMBEDDED, task);
    }
    Ok(task.to_string())
}

/// Parses a connectivity CSV text (JasperGold `CONNECTION`/`CONDITION`/`LATENCY`
/// or simple `name, src, dst [, cond [, delay]]`) relative to `top`.
pub fn parse_conn_csv(text: &str, top: Option<&str>) -> Result<Vec<ConnRule>, String> {
    struct Pending {
        name: String,
        src: String,
        dst: String,
        cond_rows: Vec<(String, Vec<String>)>,
        simple_cond: Option<String>,
        latency: (u32, u32),
        line: u32,
    }

    let finalize = |p: Pending| -> ConnRule {
        let cond = if !p.cond_rows.is_empty() {
            build_condition_matrix(&p.cond_rows)
        } else {
            p.simple_cond
        };
        ConnRule {
            name: p.name,
            src: p.src,
            dst: p.dst,
            cond,
            latency: p.latency,
            line: p.line,
        }
    };

    let mut out = Vec::new();
    let mut cur: Option<Pending> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let line = strip_line_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        let cols = split_csv_row(line);
        if cols.iter().all(|c| c.is_empty()) {
            continue;
        }
        let tag = cols[0].as_str();
        if tag.eq_ignore_ascii_case("CONNECTION") {
            if let Some(prev) = cur.take() {
                out.push(finalize(prev));
            }
            if cols.len() < 6 {
                return Err(format!(
                    "line {line_no}: CONNECTION row needs at least 6 columns (CONNECTION, name, src_blk, src_sig, dst_blk, dst_sig)"
                ));
            }
            let name = sanitize_prop_name(&cols[1], line_no);
            let src = qualify_sig(top, &cols[2], &cols[3]);
            let dst = qualify_sig(top, &cols[4], &cols[5]);
            if src.is_empty() || dst.is_empty() {
                return Err(format!(
                    "line {line_no}: CONNECTION `{name}` has empty source or destination signal"
                ));
            }
            let latency = match cols.get(6).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                Some(d) => parse_latency_spec(d, line_no)?,
                None => (0, 0),
            };
            cur = Some(Pending {
                name,
                src,
                dst,
                cond_rows: Vec::new(),
                simple_cond: None,
                latency,
                line: line_no,
            });
        } else if tag.eq_ignore_ascii_case("CONDITION") {
            let Some(p) = cur.as_mut() else {
                return Err(format!(
                    "line {line_no}: CONDITION row without a preceding CONNECTION row"
                ));
            };
            if cols.len() < 4 {
                return Err(format!(
                    "line {line_no}: CONDITION row needs at least 4 columns (CONDITION, blk, sig, val...)"
                ));
            }
            let sig = qualify_sig(top, &cols[1], &cols[2]);
            if sig.is_empty() {
                return Err(format!("line {line_no}: CONDITION row has empty signal"));
            }
            p.cond_rows.push((sig, cols[3..].to_vec()));
        } else if tag.eq_ignore_ascii_case("LATENCY") || tag.eq_ignore_ascii_case("DELAY") {
            let Some(p) = cur.as_mut() else {
                return Err(format!(
                    "line {line_no}: {tag} row without a preceding CONNECTION row"
                ));
            };
            let min_s = cols.get(1).map(|s| s.trim()).unwrap_or("");
            let max_s = cols
                .get(2)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .unwrap_or(min_s);
            let min_d = min_s
                .parse::<u32>()
                .map_err(|_| format!("line {line_no}: invalid latency `{min_s}`"))?;
            let max_d = max_s
                .parse::<u32>()
                .map_err(|_| format!("line {line_no}: invalid latency `{max_s}`"))?;
            if max_d < min_d || max_d > 64 {
                return Err(format!(
                    "line {line_no}: unsupported latency range {min_d}..{max_d}"
                ));
            }
            p.latency = (min_d, max_d);
        } else if is_header_row(&cols) {
            continue;
        } else {
            if let Some(prev) = cur.take() {
                out.push(finalize(prev));
            }
            if cols.len() < 3 {
                return Err(format!(
                    "line {line_no}: expected `CONNECTION, ...` or `name, src, dst [, cond [, delay]]`"
                ));
            }
            let name = sanitize_prop_name(&cols[0], line_no);
            let src = strip_top_prefix(top, &cols[1]);
            let dst = strip_top_prefix(top, &cols[2]);
            if src.is_empty() || dst.is_empty() {
                return Err(format!(
                    "line {line_no}: connection `{name}` has empty src or dst"
                ));
            }
            let simple_cond = cols
                .get(3)
                .map(|s| strip_top_prefix(top, s))
                .filter(|s| !s.is_empty() && s != "1" && s != "1'b1" && s != "*");
            let latency = match cols.get(4).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                Some(d) => parse_latency_spec(d, line_no)?,
                None => (0, 0),
            };
            out.push(ConnRule {
                name,
                src,
                dst,
                cond: simple_cond,
                latency,
                line: line_no,
            });
        }
    }
    if let Some(prev) = cur.take() {
        out.push(finalize(prev));
    }
    Ok(out)
}

/// Loads `path` into `task` on `s`, returning the qualified names of added assertions.
pub fn load_csv_into_session(
    s: &mut Session,
    path: &Path,
    task: &str,
) -> Result<Vec<String>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("check_conn: {}: {e}", path.display()))?;
    let top = s.design.top.clone();
    let rules = parse_conn_csv(&raw, top.as_deref())
        .map_err(|e| format!("check_conn: {}: {e}", path.display()))?;
    if rules.is_empty() {
        return Err(format!(
            "check_conn: {}: no connectivity rules found",
            path.display()
        ));
    }
    let task = ensure_conn_task(s, task)?;
    let csv_file = path.to_string_lossy().into_owned();
    let mut added = Vec::with_capacity(rules.len());
    for rule in rules {
        let expr = rule.to_sva_expr();
        let full_name = s
            .add_property_in(&task, Kind::Assert, Some(rule.name.clone()), &expr)
            .map_err(|e| {
                format!(
                    "check_conn ({}:{} `{}`): {e}",
                    path.display(),
                    rule.line,
                    rule.name
                )
            })?;
        if let Some(p) = s.props.iter_mut().find(|p| p.name == full_name) {
            p.source = Some(SourceLoc::new(&csv_file, rule.line));
        }
        added.push(full_name);
    }
    Ok(added)
}

fn build_condition_matrix(rows: &[(String, Vec<String>)]) -> Option<String> {
    let max_cols = rows.iter().map(|(_, vals)| vals.len()).max().unwrap_or(0);
    let mut minterms = Vec::new();
    for c in 0..max_cols {
        let mut literals = Vec::new();
        for (sig, vals) in rows {
            let Some(val) = vals.get(c).map(|v| v.trim()) else {
                continue;
            };
            if val.is_empty() || val == "*" || val == "-" || val.eq_ignore_ascii_case("x") {
                continue;
            }
            literals.push(format!("(({sig}) == ({val}))"));
        }
        if !literals.is_empty() {
            if literals.len() == 1 {
                minterms.push(literals.into_iter().next().unwrap());
            } else {
                minterms.push(format!("({})", literals.join(" && ")));
            }
        }
    }
    if minterms.is_empty() {
        None
    } else if minterms.len() == 1 {
        Some(minterms.into_iter().next().unwrap())
    } else {
        Some(minterms.join(" || "))
    }
}

fn qualify_sig(top: Option<&str>, blk: &str, sig: &str) -> String {
    let blk = strip_top_prefix(top, blk.trim());
    let sig = strip_top_prefix(top, sig.trim());
    if blk.is_empty() {
        sig
    } else if sig.is_empty() {
        blk
    } else {
        format!("{blk}.{sig}")
    }
}

fn strip_top_prefix(top: Option<&str>, raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('.');
    if let Some(t) = top.filter(|t| !t.is_empty()) {
        if trimmed == t {
            return String::new();
        }
        if let Some(rest) = trimmed.strip_prefix(t).and_then(|r| r.strip_prefix('.')) {
            return rest.to_string();
        }
    }
    trimmed.to_string()
}

fn sanitize_prop_name(raw: &str, line_no: u32) -> String {
    let s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        format!("conn_{line_no}")
    } else {
        s
    }
}

fn parse_latency_spec(spec: &str, line_no: u32) -> Result<(u32, u32), String> {
    let s = spec.trim();
    let (a, b) = if let Some((lo, hi)) = s.split_once(':').or_else(|| s.split_once("..")) {
        (lo.trim(), hi.trim())
    } else {
        (s, s)
    };
    let min_d = a
        .parse::<u32>()
        .map_err(|_| format!("line {line_no}: invalid delay `{s}`"))?;
    let max_d = b
        .parse::<u32>()
        .map_err(|_| format!("line {line_no}: invalid delay `{s}`"))?;
    if max_d < min_d || max_d > 64 {
        return Err(format!(
            "line {line_no}: unsupported delay `{s}` (0..64 supported)"
        ));
    }
    Ok((min_d, max_d))
}

fn is_header_row(cols: &[String]) -> bool {
    let first = cols
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        first.as_str(),
        "type" | "name" | "connection_name" | "rule" | "id" | "check_name"
    ) && cols
        .get(1)
        .map(|s| s.to_ascii_lowercase())
        .is_some_and(|s| s.contains("src") || s.contains("source") || s.contains("name"))
}

fn strip_line_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_quotes = !in_quotes,
            b'#' if !in_quotes => return &line[..i],
            b'/' if !in_quotes && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

fn split_csv_row(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                cols.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    cols.push(cur.trim().to_string());
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opentitan_style_connection_and_condition_matrix() {
        let csv = "\
# OpenTitan-style connectivity CSV
CONNECTION, scanmode_chk, top_earlgrey.u_ast, scan_mode_i, top_earlgrey, scanmode_io
CONDITION, , scan_rst_ni, 1'b1, 1'b0
CONDITION, top_earlgrey.u_pinmux, lc_dft_en, 4'b0101, 4'b1010

CONNECTION, pipe_chk, u_tx, data_o, u_rx, data_i
LATENCY, 1, 1
";
        let rules = parse_conn_csv(csv, Some("top_earlgrey")).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].name, "scanmode_chk");
        assert_eq!(rules[0].src, "u_ast.scan_mode_i");
        assert_eq!(rules[0].dst, "scanmode_io");
        assert_eq!(
            rules[0].to_sva_expr(),
            "((((scan_rst_ni) == (1'b1)) && ((u_pinmux.lc_dft_en) == (4'b0101))) || (((scan_rst_ni) == (1'b0)) && ((u_pinmux.lc_dft_en) == (4'b1010)))) |-> ((scanmode_io) == (u_ast.scan_mode_i))"
        );
        assert_eq!(
            rules[1].to_sva_expr(),
            "1'b1 |-> ##1 ((u_rx.data_i) == $past(u_tx.data_o, 1))"
        );
    }
}
