//! Automated CSR / Register-Block Formal Verification App (`check_csr`).
//!
//! Ingests **OpenTitan HJSON/JSON**, **CSV**, or **UVM RAL SystemVerilog
//! (`uvm_reg_block` / `uvm_reg`)** register specifications, resolves each
//! register/field against the elaborated RTL netlist, and populates task `CSR`
//! (or `-task <name>`) with formal SVA assertions verifying:
//! 1. Post-reset values (`csr_<reg>_<field>_reset`)
//! 2. Read-back multiplexing & `WO` read-zero masking (`csr_<reg>_<field>_read`)
//! 3. Software access policies: `RW`, `RO`, `W1C`/`RW1C`, `W1S`/`RW1S`,
//!    `W0C`/`RW0C`, `WO`, `RC` (`csr_<reg>_<field>_access`)
//! 4. Write-enable lock protection (`regwen`, `csr_<reg>_<field>_lock`)
//! 5. Address decode non-aliasing (`csr_<reg>_<field>_no_alias`)

use crate::conn::ensure_conn_task;
use crate::session::{Kind, Session, SourceLoc};
use informal_svlang::ral::{extract_ral_from_files, parse_sv_u64};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

pub const DEFAULT_CSR_TASK: &str = "CSR";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CsrFieldSpec {
    pub name: String,
    pub lsb: u32,
    pub width: u32,
    pub access: String,
    pub reset: u64,
    pub has_reset: bool,
    pub volatile: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtl_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CsrRegSpec {
    pub name: String,
    pub offset: u64,
    pub reg_width: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regwen: Option<String>,
    pub fields: Vec<CsrFieldSpec>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CsrBusConfig {
    pub addr: Option<String>,
    pub wdata: Option<String>,
    pub rdata: Option<String>,
    pub we: Option<String>,
    pub re: Option<String>,
    pub prefix: String,
    pub read_latency: u32,
}

/// Converts OpenTitan-style HJSON text (`//` & `#` & `/* */` comments, unquoted
/// keys, unquoted identifier/hex values, trailing commas) into strict JSON.
pub fn normalize_hjson_to_json(input: &str) -> String {
    // Pass 1: strip `//`, `#`, and `/* ... */` comments outside quoted strings.
    let bytes = input.as_bytes();
    let mut no_comments = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            no_comments.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                i += 1;
                no_comments.push(bytes[i] as char);
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            no_comments.push('"');
            i += 1;
        } else if b == b'#' || (b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    no_comments.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            no_comments.push(b as char);
            i += 1;
        }
    }

    // Pass 2: quote unquoted keys & bareword values, insert missing commas at newlines,
    // and strip trailing commas before `}` or `]`.
    #[derive(Clone, Debug)]
    enum Tok {
        Sym(char),
        Str(String),
        Raw(String),
        Newline,
    }
    let cb = no_comments.as_bytes();
    let mut toks: Vec<Tok> = Vec::new();
    let mut j = 0;
    while j < cb.len() {
        let ch = cb[j] as char;
        if ch == '\n' {
            toks.push(Tok::Newline);
            j += 1;
        } else if ch.is_ascii_whitespace() {
            j += 1;
        } else if matches!(ch, '{' | '}' | '[' | ']' | ':' | ',') {
            toks.push(Tok::Sym(ch));
            j += 1;
        } else if ch == '"' || ch == '\'' {
            let quote = ch;
            j += 1;
            let mut s = String::new();
            while j < cb.len() && cb[j] as char != quote {
                if cb[j] == b'\\' && j + 1 < cb.len() {
                    j += 1;
                    s.push(cb[j] as char);
                } else {
                    s.push(cb[j] as char);
                }
                j += 1;
            }
            if j < cb.len() {
                j += 1;
            }
            toks.push(Tok::Str(s));
        } else {
            let start = j;
            while j < cb.len() {
                let c = cb[j] as char;
                if c.is_ascii_whitespace() || matches!(c, '{' | '}' | '[' | ']' | ':' | ',') {
                    break;
                }
                j += 1;
            }
            toks.push(Tok::Raw(no_comments[start..j].to_string()));
        }
    }

    // Materialize JSON tokens (without newlines, adding implied commas between value and next key/elem).
    let mut clean: Vec<String> = Vec::new();
    let next_non_nl =
        |from: usize| -> Option<&Tok> { toks[from..].iter().find(|t| !matches!(t, Tok::Newline)) };
    let mut saw_nl = false;
    for (idx, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Newline => {
                saw_nl = true;
            }
            other => {
                if saw_nl {
                    let prev_ends_val = clean.last().is_some_and(|p| {
                        p == "}"
                            || p == "]"
                            || p.starts_with('"')
                            || p.chars()
                                .next()
                                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
                    });
                    let next_starts_item = !matches!(other, Tok::Sym('}' | ']' | ',' | ':'));
                    if prev_ends_val && next_starts_item {
                        clean.push(",".into());
                    }
                    saw_nl = false;
                }
                match other {
                    Tok::Sym(c) => {
                        if matches!(c, '}' | ']') && clean.last().map(String::as_str) == Some(",") {
                            clean.pop();
                        }
                        clean.push(c.to_string());
                    }
                    Tok::Str(s) => {
                        clean.push(serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\"")));
                    }
                    Tok::Raw(r) => {
                        let is_key = matches!(next_non_nl(idx + 1), Some(Tok::Sym(':')));
                        let is_json_lit = !is_key
                            && (r == "true"
                                || r == "false"
                                || r == "null"
                                || r.parse::<i64>().is_ok()
                                || r.parse::<f64>().is_ok());
                        if is_json_lit {
                            clean.push(r.clone());
                        } else {
                            clean.push(
                                serde_json::to_string(r).unwrap_or_else(|_| format!("\"{r}\"")),
                            );
                        }
                    }
                    Tok::Newline => unreachable!(),
                }
            }
        }
    }
    // Final sweep removing any `, }` or `, ]`.
    let mut out = String::new();
    for idx in 0..clean.len() {
        if clean[idx] == "," && matches!(clean.get(idx + 1).map(String::as_str), Some("}" | "]")) {
            continue;
        }
        out.push_str(&clean[idx]);
        if matches!(clean[idx].as_str(), ":" | ",") {
            out.push(' ');
        }
    }
    out
}

fn parse_bit_range(spec: &str) -> Result<(u32, u32), String> {
    let s = spec.trim().trim_matches(['[', ']']);
    if let Some((hi_s, lo_s)) = s.split_once(':') {
        let hi: u32 = hi_s
            .trim()
            .parse()
            .map_err(|_| format!("bad bit range `{spec}`"))?;
        let lo: u32 = lo_s
            .trim()
            .parse()
            .map_err(|_| format!("bad bit range `{spec}`"))?;
        if hi < lo {
            return Err(format!(
                "descending bit range [hi:lo] expected, got `{spec}`"
            ));
        }
        Ok((lo, hi - lo + 1))
    } else {
        let bit: u32 = s.parse().map_err(|_| format!("bad bit index `{spec}`"))?;
        Ok((bit, 1))
    }
}

fn json_u64(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::Bool(b) => Some(u64::from(*b)),
        serde_json::Value::String(s) => parse_sv_u64(s),
        _ => None,
    }
}

fn json_bool(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        serde_json::Value::Number(n) => n.as_u64().map(|x| x != 0),
        _ => None,
    }
}

/// Parses an OpenTitan HJSON/JSON CSR specification into [`CsrRegSpec`] entries.
pub fn parse_hjson_csr(text: &str) -> Result<Vec<CsrRegSpec>, String> {
    let normalized = normalize_hjson_to_json(text);
    let root: serde_json::Value = serde_json::from_str(&normalized)
        .map_err(|e| format!("HJSON/JSON CSR parse error: {e}"))?;
    let regwidth = root.get("regwidth").and_then(json_u64).unwrap_or(32) as u32;
    let addr_step = u64::from((regwidth / 8).max(1));

    let regs_val = root
        .get("registers")
        .and_then(|v| v.as_array())
        .ok_or("HJSON CSR spec missing `registers` array")?;

    let mut cur_offset: u64 = 0;
    let mut out = Vec::new();

    let mut visit_list = |items: &[serde_json::Value]| -> Result<(), String> {
        for item in items {
            if let Some(skip) = item.get("skipto").and_then(json_u64) {
                cur_offset = skip;
                continue;
            }
            if let Some(sub) = item.get("multireg") {
                let cname = sub
                    .get("cname")
                    .or_else(|| sub.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("REG");
                let count = sub.get("count").and_then(json_u64).unwrap_or(1);
                let sw = sub
                    .get("swaccess")
                    .and_then(|v| v.as_str())
                    .unwrap_or("rw")
                    .to_ascii_uppercase();
                let resval = sub.get("resval").and_then(json_u64).unwrap_or(0);
                let regwen = sub.get("regwen").and_then(|v| v.as_str()).map(String::from);
                for idx in 0..count {
                    out.push(CsrRegSpec {
                        name: format!("{cname}_{idx}"),
                        offset: cur_offset,
                        reg_width: regwidth,
                        regwen: regwen.clone(),
                        fields: vec![CsrFieldSpec {
                            name: format!("{cname}_{idx}"),
                            lsb: 0,
                            width: regwidth,
                            access: sw.clone(),
                            reset: resval,
                            has_reset: sub.get("resval").is_some(),
                            volatile: sub
                                .get("hwqe")
                                .or_else(|| sub.get("hwext"))
                                .and_then(json_bool)
                                .unwrap_or(false),
                            rtl_path: None,
                        }],
                    });
                    cur_offset += addr_step;
                }
                continue;
            }

            let Some(rname) = item.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(explicit_off) = item.get("offset").and_then(json_u64) {
                cur_offset = explicit_off;
            }
            let def_sw = item
                .get("swaccess")
                .and_then(|v| v.as_str())
                .unwrap_or("rw")
                .to_ascii_uppercase();
            let def_res = item.get("resval").and_then(json_u64);
            let hwext = item.get("hwext").and_then(json_bool).unwrap_or(false);
            let regwen = item
                .get("regwen")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            let reg_rtl = item
                .get("rtl_path")
                .and_then(|v| v.as_str())
                .map(String::from);

            let mut fields = Vec::new();
            if let Some(farr) = item.get("fields").and_then(|v| v.as_array()) {
                for f in farr {
                    let bits_s = f
                        .get("bits")
                        .and_then(|v| {
                            v.as_str()
                                .map(String::from)
                                .or_else(|| v.as_u64().map(|n| n.to_string()))
                        })
                        .ok_or_else(|| format!("register `{rname}` field missing `bits`"))?;
                    let (lsb, width) = parse_bit_range(&bits_s)?;
                    let fname = f
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(rname)
                        .to_string();
                    let access = f
                        .get("swaccess")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_ascii_uppercase())
                        .unwrap_or_else(|| def_sw.clone());
                    let fres = f.get("resval").and_then(json_u64).or(def_res);
                    let volatile = hwext
                        || f.get("volatile")
                            .or_else(|| f.get("hwaccess"))
                            .and_then(|v| {
                                json_bool(v).or_else(|| {
                                    v.as_str().map(|s| {
                                        matches!(s.to_ascii_lowercase().as_str(), "hwo" | "hrw")
                                    })
                                })
                            })
                            .unwrap_or(false);
                    let rtl_path = f.get("rtl_path").and_then(|v| v.as_str()).map(String::from);
                    fields.push(CsrFieldSpec {
                        name: fname,
                        lsb,
                        width,
                        access,
                        reset: fres.unwrap_or(0),
                        has_reset: fres.is_some() || item.get("resval").is_none(),
                        volatile,
                        rtl_path,
                    });
                }
            } else {
                let (lsb, width) = match item.get("bits").and_then(|v| v.as_str()) {
                    Some(b) => parse_bit_range(b)?,
                    None => (0, regwidth),
                };
                fields.push(CsrFieldSpec {
                    name: rname.to_string(),
                    lsb,
                    width,
                    access: def_sw,
                    reset: def_res.unwrap_or(0),
                    has_reset: true,
                    volatile: hwext,
                    rtl_path: reg_rtl,
                });
            }

            fields.sort_by_key(|f| f.lsb);
            out.push(CsrRegSpec {
                name: rname.to_string(),
                offset: cur_offset,
                reg_width: regwidth,
                regwen,
                fields,
            });
            cur_offset += addr_step;
        }
        Ok(())
    };
    visit_list(regs_val)?;
    Ok(out)
}

/// Parses a CSV CSR specification: `register,offset,field,bits,access,reset[,rtl_path[,regwen]]`.
pub fn parse_csv_csr(text: &str) -> Result<Vec<CsrRegSpec>, String> {
    let mut by_reg: Vec<CsrRegSpec> = Vec::new();
    let mut reg_idx: HashMap<String, usize> = HashMap::new();

    for (lno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 6 {
            continue;
        }
        if lno == 0 && cols[0].eq_ignore_ascii_case("register") {
            continue;
        }
        let rname = cols[0].to_string();
        let offset = parse_sv_u64(cols[1])
            .ok_or_else(|| format!("CSV line {}: bad offset `{}`", lno + 1, cols[1]))?;
        let fname = cols[2].to_string();
        let (lsb, width) =
            parse_bit_range(cols[3]).map_err(|e| format!("CSV line {}: {e}", lno + 1))?;
        let access = cols[4].to_ascii_uppercase();
        let reset = parse_sv_u64(cols[5]).unwrap_or(0);
        let rtl_path = cols
            .get(6)
            .copied()
            .filter(|s| !s.is_empty())
            .map(String::from);
        let regwen = cols
            .get(7)
            .copied()
            .filter(|s| !s.is_empty())
            .map(String::from);

        let idx = match reg_idx.get(&rname) {
            Some(&i) => i,
            None => {
                let i = by_reg.len();
                by_reg.push(CsrRegSpec {
                    name: rname.clone(),
                    offset,
                    reg_width: 32,
                    regwen: regwen.clone(),
                    fields: Vec::new(),
                });
                reg_idx.insert(rname, i);
                i
            }
        };
        if by_reg[idx].regwen.is_none() && regwen.is_some() {
            by_reg[idx].regwen = regwen;
        }
        by_reg[idx].fields.push(CsrFieldSpec {
            name: fname,
            lsb,
            width,
            access,
            reset,
            has_reset: !cols[5].is_empty(),
            volatile: false,
            rtl_path,
        });
    }
    Ok(by_reg)
}

/// Parses a UVM RAL `.sv` file into [`CsrRegSpec`] entries.
pub fn parse_sv_ral_csr(path: &Path) -> Result<Vec<CsrRegSpec>, String> {
    let blocks = extract_ral_from_files(&[path.to_path_buf()])?;
    let Some(block) = blocks.into_iter().next() else {
        return Err(format!(
            "no uvm_reg_block or uvm_reg found in {}",
            path.display()
        ));
    };
    Ok(block
        .registers
        .into_iter()
        .map(|r| CsrRegSpec {
            name: r.name,
            offset: r.offset,
            reg_width: r.n_bits,
            regwen: None,
            fields: r
                .fields
                .into_iter()
                .map(|f| CsrFieldSpec {
                    name: f.name,
                    lsb: f.lsb,
                    width: f.width,
                    access: f.access,
                    reset: f.reset,
                    has_reset: f.has_reset,
                    volatile: f.volatile,
                    rtl_path: None,
                })
                .collect(),
        })
        .collect())
}

fn find_signal<'a>(signals: &'a [(String, u64)], candidates: &[String]) -> Option<(&'a str, u64)> {
    for cand in candidates {
        if let Some((p, w)) = signals.iter().find(|(p, _)| p == cand) {
            return Some((p.as_str(), *w));
        }
    }
    // Case-insensitive match fallback (OpenTitan uppercase HJSON names vs lowercase RTL).
    for cand in candidates {
        if let Some((p, w)) = signals.iter().find(|(p, _)| p.eq_ignore_ascii_case(cand)) {
            return Some((p.as_str(), *w));
        }
    }
    None
}

fn resolve_rtl_field(
    signals: &[(String, u64)],
    prefix: &str,
    reg: &CsrRegSpec,
    fld: &CsrFieldSpec,
) -> Option<String> {
    if let Some(ref explicit) = fld.rtl_path {
        return Some(explicit.clone());
    }
    let rn = reg.name.to_ascii_lowercase();
    let fnm = fld.name.to_ascii_lowercase();
    let direct_cands = if rn == fnm {
        vec![
            format!("{prefix}{rn}_q"),
            format!("{prefix}{rn}"),
            format!("{prefix}reg_{rn}_q"),
            format!("{prefix}reg_{rn}"),
        ]
    } else {
        vec![
            format!("{prefix}{rn}_{fnm}_q"),
            format!("{prefix}{rn}_{fnm}"),
            format!("{prefix}{fnm}_q"),
            format!("{prefix}{fnm}"),
        ]
    };
    if let Some((sig, sig_w)) = find_signal(signals, &direct_cands) {
        if sig_w == u64::from(fld.width) {
            return Some(sig.to_string());
        }
        if sig_w >= u64::from(fld.lsb + fld.width) {
            return Some(if fld.width == 1 {
                format!("{sig}[{}]", fld.lsb)
            } else {
                format!("{sig}[{}:{}]", fld.lsb + fld.width - 1, fld.lsb)
            });
        }
    }
    // Packed whole-register signal fallback (`ctrl_q[hi:lo]`).
    let reg_cands = vec![
        format!("{prefix}{rn}_q"),
        format!("{prefix}{rn}"),
        format!("{prefix}reg_{rn}_q"),
        format!("{prefix}reg_{rn}"),
    ];
    if let Some((sig, sig_w)) = find_signal(signals, &reg_cands) {
        if sig_w == u64::from(fld.width) && fld.lsb == 0 {
            return Some(sig.to_string());
        }
        if sig_w >= u64::from(fld.lsb + fld.width) {
            return Some(if fld.width == 1 {
                format!("{sig}[{}]", fld.lsb)
            } else {
                format!("{sig}[{}:{}]", fld.lsb + fld.width - 1, fld.lsb)
            });
        }
    }
    None
}

fn resolve_regwen(
    signals: &[(String, u64)],
    prefix: &str,
    all_regs: &[CsrRegSpec],
    regwen_name: &str,
) -> Option<String> {
    if let Some(rw_reg) = all_regs
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(regwen_name))
    {
        if let Some(f0) = rw_reg.fields.first() {
            if let Some(resolved) = resolve_rtl_field(signals, prefix, rw_reg, f0) {
                return Some(resolved);
            }
        }
    }
    let low = regwen_name.to_ascii_lowercase();
    let cands = vec![
        regwen_name.to_string(),
        format!("{prefix}{low}_q"),
        format!("{prefix}{low}"),
    ];
    find_signal(signals, &cands).map(|(s, _)| s.to_string())
}

/// Loads a CSR spec file (`.hjson`, `.json`, `.csv`, or `.sv` UVM RAL) into `task`,
/// synthesizing formal reset, read-back, access-policy, `regwen` lock, and non-aliasing SVA properties.
pub fn load_csr_into_session(
    s: &mut Session,
    path: &Path,
    task: &str,
    bus: &CsrBusConfig,
) -> Result<Vec<String>, String> {
    if !s.elaborated && !s.dry_run {
        s.elaborate(None).map_err(|e| format!("check_csr: {e:#}"))?;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let specs = match ext.as_str() {
        "sv" | "svh" => parse_sv_ral_csr(path)?,
        "csv" => {
            let txt = std::fs::read_to_string(path)
                .map_err(|e| format!("check_csr: {}: {e}", path.display()))?;
            parse_csv_csr(&txt)?
        }
        _ => {
            let txt = std::fs::read_to_string(path)
                .map_err(|e| format!("check_csr: {}: {e}", path.display()))?;
            parse_hjson_csr(&txt)?
        }
    };
    if specs.is_empty() {
        return Err(format!(
            "check_csr: no registers found in {}",
            path.display()
        ));
    }

    let top = s.design.top.clone().unwrap_or_default();
    let top_prefix = format!("{top}.");
    let rel_signals: Vec<(String, u64)> = s
        .elab
        .as_ref()
        .map(|d| {
            d.signals
                .iter()
                .filter(|sig| !sig.constant)
                .filter_map(|sig| {
                    sig.path
                        .strip_prefix(&top_prefix)
                        .map(|rel| (rel.to_string(), sig.width))
                })
                .collect()
        })
        .unwrap_or_default();

    let pick_bus = |explicit: &Option<String>, defaults: &[&str]| -> Option<(String, u64)> {
        if let Some(ex) = explicit {
            let w = rel_signals
                .iter()
                .find(|(n, _)| n == ex)
                .map_or(32, |(_, w)| *w);
            return Some((ex.clone(), w));
        }
        let cands: Vec<String> = defaults.iter().map(|s| (*s).to_string()).collect();
        find_signal(&rel_signals, &cands).map(|(p, w)| (p.to_string(), w))
    };

    let (addr_sig, addr_w) = pick_bus(
        &bus.addr,
        &["addr", "reg_addr", "csr_addr", "paddr", "a_address"],
    )
    .ok_or("check_csr: could not auto-detect CSR address bus; pass `-addr <sig>`")?;
    let wdata_sig = pick_bus(
        &bus.wdata,
        &["wdata", "reg_wdata", "csr_wdata", "pwdata", "a_data"],
    )
    .map(|(n, _)| n);
    let rdata_sig = pick_bus(
        &bus.rdata,
        &["rdata", "reg_rdata", "csr_rdata", "prdata", "d_data"],
    )
    .map(|(n, _)| n);
    let we_expr = bus
        .we
        .clone()
        .or_else(|| pick_bus(&None, &["we", "reg_we", "csr_we", "wr_en"]).map(|(n, _)| n));
    let re_expr = bus
        .re
        .clone()
        .or_else(|| pick_bus(&None, &["re", "reg_re", "csr_re", "rd_en"]).map(|(n, _)| n));

    let task = ensure_conn_task(s, task)?;
    let reset_cond = s.reset.clone();
    let spec_file = path.to_string_lossy().into_owned();
    let mut added = Vec::new();

    for reg in &specs {
        let addr_lit = format!("{addr_w}'d{}", reg.offset);
        let regwen_resolved = reg
            .regwen
            .as_deref()
            .and_then(|rw| resolve_regwen(&rel_signals, &bus.prefix, &specs, rw));

        for fld in &reg.fields {
            let Some(rtl_fld) = resolve_rtl_field(&rel_signals, &bus.prefix, reg, fld) else {
                return Err(format!(
                    "check_csr: could not map register `{}` field `{}` to an RTL signal in `{top}` (use `-prefix` or `rtl_path`)",
                    reg.name, fld.name
                ));
            };
            let base = if reg.name.eq_ignore_ascii_case(&fld.name) {
                format!("csr_{}", reg.name.to_ascii_lowercase())
            } else {
                format!(
                    "csr_{}_{}",
                    reg.name.to_ascii_lowercase(),
                    fld.name.to_ascii_lowercase()
                )
            };
            let fw = fld.width;
            let hi = fld.lsb + fw - 1;
            let lo = fld.lsb;
            let mask = if fw >= 64 { u64::MAX } else { (1u64 << fw) - 1 };
            let rst_val = fld.reset & mask;

            let mut add_one = |suffix: &str, expr: String| -> Result<(), String> {
                let pname = format!("{base}_{suffix}");
                let qname = s.add_property_in(&task, Kind::Assert, Some(pname), &expr)?;
                if let Some(p) = s.props.iter_mut().find(|p| p.name == qname) {
                    p.source = Some(SourceLoc::new(&spec_file, 1));
                }
                added.push(qname);
                Ok(())
            };

            // 1. Post-reset value check on first cycle leaving reset.
            if fld.has_reset {
                if let Some(ref rst_cond) = reset_cond {
                    add_one(
                        "reset",
                        format!("$fell({rst_cond}) |-> (({rtl_fld}) == {fw}'d{rst_val})"),
                    )?;
                }
            }

            // 2. Read-back check (`WO` reads 0; non-volatile readable fields return `rtl_fld`).
            if !fld.volatile {
                if let (Some(ref rd), Some(ref re)) = (&rdata_sig, &re_expr) {
                    let rslice = if fw == 1 {
                        format!("{rd}[{lo}]")
                    } else {
                        format!("{rd}[{hi}:{lo}]")
                    };
                    let rd_hit = match &we_expr {
                        Some(we) => format!("(({re}) && (({addr_sig}) == {addr_lit}) && !({we}))"),
                        None => format!("(({re}) && (({addr_sig}) == {addr_lit}))"),
                    };
                    let expected_now = if fld.access == "WO" {
                        format!("{fw}'d0")
                    } else {
                        rtl_fld.clone()
                    };
                    let expected_past = if fld.access == "WO" {
                        format!("{fw}'d0")
                    } else {
                        format!("$past({rtl_fld})")
                    };
                    if bus.read_latency == 0 {
                        add_one(
                            "read",
                            format!("{rd_hit} |-> (({rslice}) == ({expected_now}))"),
                        )?;
                    } else {
                        add_one(
                            "read",
                            format!("{rd_hit} |=> (({rslice}) == ({expected_past}))"),
                        )?;
                    }
                }
            }

            // 3. Software write / read-clear access policy + 4. `regwen` lock + 5. Non-aliasing.
            if !fld.volatile {
                if let (Some(ref wd), Some(ref we)) = (&wdata_sig, &we_expr) {
                    let wslice = if fw == 1 {
                        format!("{wd}[{lo}]")
                    } else {
                        format!("{wd}[{hi}:{lo}]")
                    };
                    let wr_hit = format!("(({we}) && (({addr_sig}) == {addr_lit}))");
                    let wr_unlocked = match &regwen_resolved {
                        Some(rw) => format!("({wr_hit} && ({rw}))"),
                        None => wr_hit.clone(),
                    };

                    match fld.access.as_str() {
                        "RW" | "WO" => {
                            add_one(
                                "access",
                                format!("{wr_unlocked} |=> (({rtl_fld}) == $past({wslice}))"),
                            )?;
                        }
                        "RO" => {
                            add_one(
                                "access",
                                format!("{wr_hit} |=> (({rtl_fld}) == $past({rtl_fld}))"),
                            )?;
                        }
                        "W1C" | "RW1C" => {
                            add_one(
                                "access",
                                format!(
                                    "{wr_unlocked} |=> (({rtl_fld}) == ($past({rtl_fld}) & ~$past({wslice})))"
                                ),
                            )?;
                        }
                        "W1S" | "RW1S" => {
                            add_one(
                                "access",
                                format!(
                                    "{wr_unlocked} |=> (({rtl_fld}) == ($past({rtl_fld}) | $past({wslice})))"
                                ),
                            )?;
                        }
                        "W0C" | "RW0C" => {
                            add_one(
                                "access",
                                format!(
                                    "{wr_unlocked} |=> (({rtl_fld}) == ($past({rtl_fld}) & $past({wslice})))"
                                ),
                            )?;
                        }
                        _ => {}
                    }

                    if matches!(
                        fld.access.as_str(),
                        "RW" | "WO" | "W1C" | "RW1C" | "W1S" | "RW1S" | "W0C" | "RW0C"
                    ) {
                        if let Some(ref rw) = regwen_resolved {
                            add_one(
                                "lock",
                                format!(
                                    "({wr_hit} && !({rw})) |=> (({rtl_fld}) == $past({rtl_fld}))"
                                ),
                            )?;
                        }
                    }

                    if fld.access != "RC" {
                        add_one(
                            "no_alias",
                            format!(
                                "(({we}) && (({addr_sig}) != {addr_lit})) |=> (({rtl_fld}) == $past({rtl_fld}))"
                            ),
                        )?;
                    }
                }
                if fld.access == "RC" {
                    if let Some(ref re) = re_expr {
                        let rd_hit = match &we_expr {
                            Some(we) => {
                                format!("(({re}) && (({addr_sig}) == {addr_lit}) && !({we}))")
                            }
                            None => format!("(({re}) && (({addr_sig}) == {addr_lit}))"),
                        };
                        add_one("access", format!("{rd_hit} |=> (({rtl_fld}) == {fw}'d0)"))?;
                    }
                }
            }
        }
    }

    Ok(added)
}

pub fn format_csr_report(s: &Session, task: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "CSR formal verification report (task `{task}`):");
    out.push_str(&s.report_of(Some(task)));
    out
}
