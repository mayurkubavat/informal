use crate::session::{Session, Status};
use std::collections::{HashMap, HashSet};

pub(super) struct ParsedVcd {
    pub(super) frames: usize,
    /// Map from full VCD signal name -> per-frame LSB-first bits.
    pub(super) by_name: HashMap<String, Vec<Vec<bool>>>,
    pub(super) clocks: HashSet<String>,
}

pub(super) fn parse_vcd_bits(vcd_text: &str) -> Result<ParsedVcd, String> {
    let mut scope: Vec<&str> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut widths: Vec<usize> = Vec::new();
    let mut by_code: HashMap<&str, usize> = HashMap::new();
    let mut clocks: HashSet<String> = HashSet::new();
    let mut declared_frames: Option<usize> = None;

    let mut lines = vcd_text.lines();
    for line in lines.by_ref() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.first().copied() {
            Some("$comment") => {
                let body = w[1..w.len().saturating_sub(1)].join(" ");
                if let Some(rest) = body.strip_prefix("informal trace of ") {
                    let count = rest.rsplit(": ").next().unwrap_or("");
                    declared_frames = count.split(' ').next().and_then(|n| n.parse().ok());
                }
            }
            Some("$scope") => scope.push(w.get(2).copied().unwrap_or("")),
            Some("$upscope") => {
                scope.pop();
            }
            Some("$var") if w.len() >= 6 => {
                let width: usize = w[2].parse().map_err(|_| format!("bad $var line: {line}"))?;
                let full = scope
                    .iter()
                    .chain([&w[4]])
                    .copied()
                    .collect::<Vec<_>>()
                    .join(".");
                if names.is_empty() {
                    clocks.insert(full.clone());
                }
                by_code.insert(w[3], names.len());
                names.push(full);
                widths.push(width);
            }
            Some("$enddefinitions") => break,
            _ => {}
        }
    }

    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut cur_frame: Option<usize> = None;
    let mut max_frame = 0usize;
    for line in lines {
        let l = line.trim();
        if l.is_empty() || l.starts_with('$') {
            continue;
        }
        if let Some(ts) = l.strip_prefix('#') {
            if let Ok(t) = ts.parse::<usize>() {
                if t % 10 == 0 {
                    let f = t / 10;
                    cur_frame = Some(f);
                    max_frame = max_frame.max(f);
                } else {
                    cur_frame = None;
                }
            }
            continue;
        }
        let Some(f) = cur_frame else { continue };
        if let Some(rest) = l.strip_prefix('b').or_else(|| l.strip_prefix('B')) {
            if let Some((bits, code)) = rest.split_once(' ') {
                if let Some(&idx) = by_code.get(code.trim()) {
                    changes.push((f, idx, bits.to_string()));
                }
            }
        } else {
            let mut chars = l.chars();
            if let Some(bch) = chars.next() {
                let code = chars.as_str().trim();
                if let Some(&idx) = by_code.get(code) {
                    changes.push((f, idx, bch.to_string()));
                }
            }
        }
    }

    let total_frames = declared_frames.unwrap_or(max_frame + 1).max(max_frame + 1);
    let mut history: Vec<Vec<Vec<bool>>> = widths
        .iter()
        .map(|&w| vec![vec![false; w.max(1)]; total_frames])
        .collect();
    let mut cur: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w.max(1)]).collect();
    let mut ptr = 0usize;
    for f in 0..total_frames {
        while ptr < changes.len() && changes[ptr].0 == f {
            let (_, idx, ref msb_str) = changes[ptr];
            cur[idx] = msb_str_to_lsb(msb_str, widths[idx].max(1));
            ptr += 1;
        }
        for (hist_sig, cur_sig) in history.iter_mut().zip(&cur) {
            hist_sig[f] = cur_sig.clone();
        }
    }

    let mut by_name = HashMap::with_capacity(names.len());
    for (name, hist) in names.into_iter().zip(history) {
        by_name.insert(name, hist);
    }
    Ok(ParsedVcd {
        frames: total_frames,
        by_name,
        clocks,
    })
}

pub(super) fn msb_str_to_lsb(msb: &str, width: usize) -> Vec<bool> {
    let mut out = vec![false; width];
    let bytes = msb.as_bytes();
    for (bit_pos, slot) in out.iter_mut().enumerate() {
        if bit_pos < bytes.len() {
            *slot = bytes[bytes.len() - 1 - bit_pos] == b'1';
        }
    }
    out
}

pub(super) fn is_helper_name(name: &str) -> bool {
    let leaf = name.rsplit('.').next().unwrap_or(name);
    name.starts_with("informal.")
        || leaf.starts_with("informal_")
        || leaf.starts_with("_sva_")
        || leaf.starts_with("miter_")
}

pub(super) fn lookup_vcd<'a>(
    vcd: &'a ParsedVcd,
    sym: &str,
    top_prefix: &str,
) -> Option<&'a Vec<Vec<bool>>> {
    let leaf = sym.rsplit('.').next().unwrap_or(sym);
    vcd.by_name
        .get(sym)
        .or_else(|| {
            if !sym.contains('.') {
                let prefix = if top_prefix.is_empty() {
                    "top"
                } else {
                    top_prefix
                };
                vcd.by_name.get(&format!("{prefix}.{sym}"))
            } else {
                None
            }
        })
        .or_else(|| vcd.by_name.get(&format!("informal.{leaf}")))
}

fn btor_paths_from_trace(
    workdir: &std::path::Path,
    trace_path: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let tp = std::path::Path::new(trace_path);
    let mut trace_btor = tp.with_extension("trace.btor2");
    let mut plain_btor = tp.with_extension("btor2");
    if !trace_btor.is_file() && !plain_btor.is_file() {
        if let Some(tb_name) = trace_btor.file_name() {
            let alt = workdir.join(tb_name);
            if alt.is_file() {
                trace_btor = alt;
            }
        }
        if let Some(pb_name) = plain_btor.file_name() {
            let alt = workdir.join(pb_name);
            if alt.is_file() {
                plain_btor = alt;
            }
        }
    }
    (trace_btor, plain_btor)
}

pub(super) type ResolvedTarget = (
    String,
    Status,
    String,
    std::path::PathBuf,
    std::path::PathBuf,
);

pub(super) fn resolve_sec_target(s: &Session, key: &str) -> Option<Result<ResolvedTarget, String>> {
    let sec_chk = s
        .sec
        .checks
        .iter()
        .find(|c| c.name == key || c.imp_sig == key)?;
    let Some(tp) = &sec_chk.trace else {
        return Some(Err(format!(
            "why_cex: SEC check '{}' has no counterexample trace (status is {})",
            sec_chk.name,
            sec_chk.status.word()
        )));
    };
    let st = match sec_chk.status {
        crate::sec::SecStatus::Cex { depth } => Status::Cex {
            engine: sec_chk.engine.clone().unwrap_or_default(),
            depth,
            ms: sec_chk.ms,
        },
        _ => Status::Undetermined {
            reason: sec_chk.status.word().to_string(),
        },
    };
    let (tb, pb) = btor_paths_from_trace(&s.workdir, tp);
    let (trace_btor, plain_btor) = if tb.is_file() || pb.is_file() {
        (tb, pb)
    } else {
        (
            s.workdir.join(format!("{}.trace.btor2", sec_chk.name)),
            s.workdir.join(format!("{}.btor2", sec_chk.name)),
        )
    };
    Some(Ok((
        sec_chk.name.clone(),
        st,
        tp.clone(),
        trace_btor,
        plain_btor,
    )))
}

pub(super) fn resolve_xprop_target(
    s: &Session,
    key: &str,
) -> Option<Result<ResolvedTarget, String>> {
    let top = s.design.top.as_deref();
    let it = s.xprop.items.iter().find(|it| {
        it.name == key
            || top.is_some_and(|t| it.name == format!("{t}.{key}"))
            || format!("xprop_{}", it.name) == key
    })?;
    let (crate::xprop::XpropStatus::Cex { depth }, Some(tp)) = (it.status, it.trace.as_ref())
    else {
        return Some(Err(format!(
            "why_cex: X-prop item '{}' has no counterexample trace (status is {})",
            it.name,
            it.status.word()
        )));
    };
    let st = Status::Cex {
        engine: it.engine.clone().unwrap_or_default(),
        depth,
        ms: it.ms,
    };
    let (trace_btor, plain_btor) = btor_paths_from_trace(&s.workdir, tp);
    Some(Ok((
        it.name.clone(),
        st,
        tp.clone(),
        trace_btor,
        plain_btor,
    )))
}

pub(super) fn resolve_spv_target(s: &Session, key: &str) -> Option<Result<ResolvedTarget, String>> {
    let r = s
        .spv
        .iter()
        .find(|r| r.name == key || format!("spv_{}", r.name) == key)?;
    let (crate::spv::SpvStatus::Cex { depth }, Some(tp)) = (&r.status, &r.trace) else {
        return Some(Err(format!(
            "why_cex: SPV rule '{}' has no counterexample trace (status is {})",
            r.name,
            r.status.word()
        )));
    };
    let st = Status::Cex {
        engine: r.engine.clone().unwrap_or_default(),
        depth: *depth,
        ms: r.ms,
    };
    let (trace_btor, plain_btor) = btor_paths_from_trace(&s.workdir, tp);
    Some(Ok((r.name.clone(), st, tp.clone(), trace_btor, plain_btor)))
}
