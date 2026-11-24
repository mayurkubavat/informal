//! Formal Sequential & Combinational Equivalence Checking (`check_sec`).
//!
//! Compiles two SystemVerilog designs (`spec` and `imp`) via `informal_svlang`
//! and constructs a unified 2-design word-level Btor2 miter:
//! - **Sequential Equivalence Checking (`SEC`)**: ties matched primary inputs
//!   (`shared.<port>`) and power-on uninitialized matched registers at step 0,
//!   steps both state machines under shared reset & environment assumptions,
//!   supports retimed/pipelined implementations via `-latency N` shift-register
//!   alignment, and proves `spec.<out>(t - N) == imp.<out>(t)` across all
//!   reachable post-reset cycles (`t >= reset_steps + N`).
//! - **Combinational Equivalence Checking (`LEC`, `-lec`)**: cuts matched state
//!   registers into shared free keypoint inputs (`keypoint.<reg>`) and checks
//!   1-step combinational equivalence of every matched next-state cone
//!   (`lec_reg_<reg>`) and primary output cone (`lec_out_<port>`).
//!
//! Falsified checks replay witnesses into side-by-side `shared.*` / `spec.*` /
//! `imp.*` VCD traces (`<workdir>/<check>.vcd`) and keep `<workdir>/<check>.trace.btor2`
//! so `why_cex` slices the exact diverging paths across both designs.

use crate::miter::{parse_full_model, resolve_signal};
use crate::session::Session;
use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_ir::{Entry, Model, Op, Sort};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SecStatus {
    Unprocessed,
    Proven,
    Cex { depth: Option<u32> },
    Undetermined,
    Error(String),
}

impl SecStatus {
    pub fn word(&self) -> &'static str {
        match self {
            Self::Unprocessed => "unprocessed",
            Self::Proven => "proven",
            Self::Cex { .. } => "cex",
            Self::Undetermined => "undetermined",
            Self::Error(_) => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SecCheck {
    pub name: String,
    /// `"sec_output"`, `"lec_state"`, or `"lec_output"`.
    pub kind: String,
    pub spec_sig: String,
    pub imp_sig: String,
    pub width: u32,
    pub latency: u32,
    pub status: SecStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SecSetup {
    pub spec_files: Vec<PathBuf>,
    pub spec_top: Option<String>,
    pub spec_defines: Vec<(String, Option<String>)>,
    pub spec_incdirs: Vec<PathBuf>,
    pub spec_params: Vec<(String, String)>,
    pub imp_files: Vec<PathBuf>,
    pub imp_top: Option<String>,
    pub imp_defines: Vec<(String, Option<String>)>,
    pub imp_incdirs: Vec<PathBuf>,
    pub imp_params: Vec<(String, String)>,
    pub clock: Option<String>,
    pub reset: Option<String>,
    pub lec: bool,
    pub latency: u32,
    pub input_map: Vec<(String, String)>,
    pub output_map: Vec<(String, String)>,
    pub state_map: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output_latency: HashMap<String, u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SecState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<SecSetup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<SecCheck>,
    #[serde(skip)]
    pub spec_compiled: Option<Arc<informal_svlang::CompiledDesign>>,
    #[serde(skip)]
    pub imp_compiled: Option<Arc<informal_svlang::CompiledDesign>>,
}

impl SecState {
    pub fn is_empty(&self) -> bool {
        self.setup.is_none() && self.checks.is_empty()
    }
}

fn strip_top<'a>(sym: &'a str, top: Option<&str>) -> &'a str {
    match top.filter(|t| !t.is_empty()) {
        Some(t) => sym
            .strip_prefix(t)
            .and_then(|r| r.strip_prefix('.'))
            .unwrap_or(sym),
        None => sym,
    }
}

fn is_internal_helper(rel: &str) -> bool {
    let leaf = rel.rsplit('.').next().unwrap_or(rel);
    rel.starts_with("informal_")
        || leaf.starts_with("informal_")
        || leaf.starts_with("_sva_")
        || leaf.starts_with("_past_")
        || leaf.starts_with("_was_")
        || leaf.starts_with("_clk_")
}

fn sanitize_ident(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn top_ports(design: &informal_svlang::Design, top: &str, want_outputs: bool) -> Vec<String> {
    design
        .hierarchy
        .iter()
        .find(|sc| sc.name == top || sc.module == top)
        .or_else(|| design.hierarchy.first())
        .map(|sc| {
            sc.signals
                .iter()
                .filter(|sig| {
                    sig.kind == "port"
                        && if want_outputs {
                            matches!(sig.dir.as_deref(), Some("output" | "inout"))
                        } else {
                            matches!(sig.dir.as_deref(), Some("input"))
                        }
                })
                .map(|sig| sig.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Validates and stores `SecSetup`, inferring `spec_top` / `imp_top` when unambiguous.
pub fn setup_sec(s: &mut Session, mut setup: SecSetup) -> Result<(String, String), String> {
    if setup.clock.is_none() {
        setup.clock = s.clocks.first().map(|c| c.signal.clone());
    }
    if setup.reset.is_none() {
        setup.reset = s.reset.clone();
    }
    if s.dry_run {
        let st = setup.spec_top.clone().unwrap_or_else(|| "spec".into());
        let it = setup.imp_top.clone().unwrap_or_else(|| "imp".into());
        s.sec.setup = Some(setup);
        s.sec.checks.clear();
        s.sec.spec_compiled = None;
        s.sec.imp_compiled = None;
        return Ok((st, it));
    }
    if setup.spec_files.is_empty() || setup.imp_files.is_empty() {
        return Err("check_sec -setup: both -spec_files (or -spec) and -imp_files (or -imp) require at least one file".into());
    }

    let req_spec = informal_svlang::Request {
        files: setup.spec_files.clone(),
        defines: setup.spec_defines.clone(),
        incdirs: setup.spec_incdirs.clone(),
        top: setup.spec_top.clone(),
        params: setup.spec_params.clone(),
        skip_initial_checks: s.design.skip_initial_checks,
        isunknown_is_false: s.design.isunknown_is_false,
        ..Default::default()
    };
    let spec_cd = informal_svlang::compile_cached(s.sec.spec_compiled.as_ref(), &req_spec)
        .map_err(|e| format!("check_sec (spec compile errors):\nerror: {e}"))?;
    let d_spec = informal_svlang::design_from_compiled(&spec_cd, &req_spec);
    let spec_errs: Vec<String> = d_spec.errors().take(10).map(|e| e.to_string()).collect();
    if !spec_errs.is_empty() {
        return Err(format!(
            "check_sec (spec compile errors):\n{}",
            spec_errs.join("\n")
        ));
    }
    if setup.spec_top.is_none() {
        match d_spec.tops.as_slice() {
            [t] => setup.spec_top = Some(t.clone()),
            tops => {
                return Err(format!(
                    "check_sec: ambiguous spec top module {tops:?}; specify -spec_top"
                ))
            }
        }
    }

    let req_imp = informal_svlang::Request {
        files: setup.imp_files.clone(),
        defines: setup.imp_defines.clone(),
        incdirs: setup.imp_incdirs.clone(),
        top: setup.imp_top.clone(),
        params: setup.imp_params.clone(),
        skip_initial_checks: s.design.skip_initial_checks,
        isunknown_is_false: s.design.isunknown_is_false,
        ..Default::default()
    };
    let imp_cd = informal_svlang::compile_cached(s.sec.imp_compiled.as_ref(), &req_imp)
        .map_err(|e| format!("check_sec (imp compile errors):\nerror: {e}"))?;
    let d_imp = informal_svlang::design_from_compiled(&imp_cd, &req_imp);
    let imp_errs: Vec<String> = d_imp.errors().take(10).map(|e| e.to_string()).collect();
    if !imp_errs.is_empty() {
        return Err(format!(
            "check_sec (imp compile errors):\n{}",
            imp_errs.join("\n")
        ));
    }
    if setup.imp_top.is_none() {
        match d_imp.tops.as_slice() {
            [t] => setup.imp_top = Some(t.clone()),
            tops => {
                return Err(format!(
                    "check_sec: ambiguous imp top module {tops:?}; specify -imp_top"
                ))
            }
        }
    }

    let st = setup.spec_top.clone().unwrap_or_default();
    let it = setup.imp_top.clone().unwrap_or_default();
    s.sec.spec_compiled = Some(spec_cd);
    s.sec.imp_compiled = Some(imp_cd);
    s.sec.setup = Some(setup);
    s.sec.checks.clear();
    Ok((st, it))
}

#[derive(Debug, Clone)]
struct DualTarget {
    name: String,
    kind: &'static str,
    spec_sig: String,
    imp_sig: String,
    spec_ref: i64,
    imp_ref: i64,
    width: u32,
    latency: u32,
}

#[allow(clippy::too_many_arguments)]
fn build_dual_miter_btor2(
    m_spec: &Model,
    spec_top: &str,
    m_imp: &Model,
    imp_top: &str,
    input_pairs: &[(u32, u32, String)], // (spec_in_id, imp_in_id, shared_rel_name)
    state_pairs: &[(u32, u32, String)], // (spec_st_id, imp_st_id, rel_name)
    lec: bool,
    skip_steps: u32,
    target: &DualTarget,
) -> Result<String, String> {
    let mut spec_init_states: HashSet<u32> = HashSet::new();
    for l in m_spec.with_op(Op::Init) {
        if let Some(&st) = l.args.first() {
            spec_init_states.insert(st.unsigned_abs() as u32);
        }
    }
    let mut imp_init_states: HashSet<u32> = HashSet::new();
    for l in m_imp.with_op(Op::Init) {
        if let Some(&st) = l.args.first() {
            imp_init_states.insert(st.unsigned_abs() as u32);
        }
    }

    let imp_to_shared_in: HashMap<u32, u32> = input_pairs.iter().map(|&(s, i, _)| (i, s)).collect();
    let spec_shared_in_name: HashMap<u32, &str> = input_pairs
        .iter()
        .map(|(s, _, n)| (*s, n.as_str()))
        .collect();

    let spec_kp_state: HashMap<u32, &str> = state_pairs
        .iter()
        .map(|(s, _, n)| (*s, n.as_str()))
        .collect();
    let imp_to_spec_state: HashMap<u32, u32> =
        state_pairs.iter().map(|&(s, i, _)| (i, s)).collect();

    let mut out = String::new();
    let mut next_id: u32 = 1;
    let mut alloc = || -> u32 {
        let id = next_id;
        next_id += 1;
        id
    };

    let mut bv_sort_by_width: HashMap<u32, u32> = HashMap::new();
    let mut arr_sort_cache: HashMap<(u32, u32), u32> = HashMap::new();
    let ensure_bv = |w: u32,
                     out: &mut String,
                     bv_map: &mut HashMap<u32, u32>,
                     alloc: &mut dyn FnMut() -> u32|
     -> u32 {
        if let Some(&sid) = bv_map.get(&w) {
            return sid;
        }
        let sid = alloc();
        let _ = writeln!(out, "{sid} sort bitvec {w}");
        bv_map.insert(w, sid);
        sid
    };

    let mut map_sorts = |m: &Model| -> Result<HashMap<u32, u32>, String> {
        let mut smap = HashMap::new();
        for entry in m.entries() {
            if let Entry::Sort { id, sort, .. } = entry {
                let new_sid = match *sort {
                    Sort::BitVec(w) => ensure_bv(w, &mut out, &mut bv_sort_by_width, &mut alloc),
                    Sort::Array { index, element } => {
                        let ix = *smap
                            .get(&index)
                            .ok_or_else(|| format!("sec: missing index sort {index}"))?;
                        let el = *smap
                            .get(&element)
                            .ok_or_else(|| format!("sec: missing element sort {element}"))?;
                        if let Some(&cached) = arr_sort_cache.get(&(ix, el)) {
                            cached
                        } else {
                            let sid = alloc();
                            let _ = writeln!(out, "{sid} sort array {ix} {el}");
                            arr_sort_cache.insert((ix, el), sid);
                            sid
                        }
                    }
                };
                smap.insert(*id, new_sid);
            }
        }
        Ok(smap)
    };

    let sort_spec = map_sorts(m_spec)?;
    let sort_imp = map_sorts(m_imp)?;
    let sort1 = ensure_bv(1, &mut out, &mut bv_sort_by_width, &mut alloc);
    let sort_target = ensure_bv(target.width, &mut out, &mut bv_sort_by_width, &mut alloc);

    let mut map_s: HashMap<u32, u32> = HashMap::new();
    let mut map_i: HashMap<u32, u32> = HashMap::new();
    let mut pending_next: Vec<(u32, u32, i64)> = Vec::new();
    let mut constraints: Vec<i64> = Vec::new();
    let mut trace_outputs: Vec<(i64, String)> = Vec::new();

    let tr = |arg: i64, map: &HashMap<u32, u32>, side: &str| -> Result<i64, String> {
        let base = arg.unsigned_abs() as u32;
        let mapped =
            *map.get(&base)
                .ok_or_else(|| format!("sec ({side}): unmapped node {base}"))? as i64;
        Ok(if arg < 0 { -mapped } else { mapped })
    };

    // 1. Emit `spec` nodes.
    for l in m_spec.lines() {
        match l.op {
            Op::Bad | Op::Justice | Op::Fair => continue,
            Op::Output => {
                if let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) {
                    let rel = strip_top(sym, Some(spec_top));
                    if !is_internal_helper(rel) {
                        if let Ok(mapped) = tr(arg, &map_s, "spec") {
                            trace_outputs.push((mapped, format!("spec.{rel}")));
                        }
                    }
                }
            }
            Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones => {
                let srt = sort_spec[&l.sort.unwrap()];
                let nid = alloc();
                let _ = write!(out, "{nid} {} {srt}", l.op.name());
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                map_s.insert(l.id, nid);
            }
            Op::Input => {
                let srt = sort_spec[&l.sort.unwrap()];
                let raw = l.symbol.as_deref().unwrap_or("in");
                let sym = if let Some(&shared_rel) = spec_shared_in_name.get(&l.id) {
                    format!("shared.{shared_rel}")
                } else {
                    format!("spec.{}", strip_top(raw, Some(spec_top)))
                };
                let nid = alloc();
                let _ = writeln!(out, "{nid} input {srt} {sym}");
                map_s.insert(l.id, nid);
            }
            Op::State => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_spec[&orig_srt];
                let is_bv = matches!(m_spec.sort(orig_srt), Some(Sort::BitVec(_)));
                if lec && is_bv {
                    if let Some(&kp_rel) = spec_kp_state.get(&l.id) {
                        let nid = alloc();
                        let _ = writeln!(out, "{nid} input {srt} keypoint.{kp_rel}");
                        map_s.insert(l.id, nid);
                        continue;
                    }
                }
                let raw = l.symbol.as_deref().unwrap_or("st");
                let rel = strip_top(raw, Some(spec_top));
                let nid = alloc();
                let _ = writeln!(out, "{nid} state {srt} spec.{rel}");
                map_s.insert(l.id, nid);
            }
            Op::Init => {
                let st_orig = l.args[0].unsigned_abs() as u32;
                if lec && spec_kp_state.contains_key(&st_orig) {
                    continue;
                }
                let srt = sort_spec[&l.sort.unwrap()];
                let st_new = map_s[&st_orig];
                let val_new = tr(l.args[1], &map_s, "spec")?;
                let nid = alloc();
                let _ = writeln!(out, "{nid} init {srt} {st_new} {val_new}");
            }
            Op::Next => {
                let st_orig = l.args[0].unsigned_abs() as u32;
                if lec && spec_kp_state.contains_key(&st_orig) {
                    continue;
                }
                let srt = sort_spec[&l.sort.unwrap()];
                let st_new = map_s[&st_orig];
                let val_new = tr(l.args[1], &map_s, "spec")?;
                pending_next.push((srt, st_new, val_new));
            }
            Op::Constraint => {
                if !lec
                    || !l
                        .symbol
                        .as_deref()
                        .is_some_and(|s| s.starts_with("svlang") && s.ends_with("_reset"))
                {
                    let c = tr(l.args[0], &map_s, "spec")?;
                    constraints.push(c);
                }
            }
            _ => {
                let srt = sort_spec[&l.sort.unwrap()];
                let args: Result<Vec<i64>, String> =
                    l.args.iter().map(|&a| tr(a, &map_s, "spec")).collect();
                let args = args?;
                let nid = alloc();
                let _ = write!(out, "{nid} {} {srt}", l.op.name());
                for a in &args {
                    let _ = write!(out, " {a}");
                }
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                map_s.insert(l.id, nid);
            }
        }
    }

    // 2. Emit `imp` nodes, sharing matched inputs (and in LEC mode, matched keypoint states).
    for l in m_imp.lines() {
        match l.op {
            Op::Bad | Op::Justice | Op::Fair => continue,
            Op::Output => {
                if let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) {
                    let rel = strip_top(sym, Some(imp_top));
                    if !is_internal_helper(rel) {
                        if let Ok(mapped) = tr(arg, &map_i, "imp") {
                            trace_outputs.push((mapped, format!("imp.{rel}")));
                        }
                    }
                }
            }
            Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones => {
                let srt = sort_imp[&l.sort.unwrap()];
                let nid = alloc();
                let _ = write!(out, "{nid} {} {srt}", l.op.name());
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                map_i.insert(l.id, nid);
            }
            Op::Input => {
                if let Some(&spec_in) = imp_to_shared_in.get(&l.id) {
                    map_i.insert(l.id, map_s[&spec_in]);
                } else {
                    let srt = sort_imp[&l.sort.unwrap()];
                    let raw = l.symbol.as_deref().unwrap_or("in");
                    let rel = strip_top(raw, Some(imp_top));
                    let nid = alloc();
                    let _ = writeln!(out, "{nid} input {srt} imp.{rel}");
                    map_i.insert(l.id, nid);
                }
            }
            Op::State => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_imp[&orig_srt];
                let is_bv = matches!(m_imp.sort(orig_srt), Some(Sort::BitVec(_)));
                if lec && is_bv {
                    if let Some(&spec_st) = imp_to_spec_state.get(&l.id) {
                        map_i.insert(l.id, map_s[&spec_st]);
                        continue;
                    }
                }
                let raw = l.symbol.as_deref().unwrap_or("st");
                let rel = strip_top(raw, Some(imp_top));
                let nid = alloc();
                let _ = writeln!(out, "{nid} state {srt} imp.{rel}");
                map_i.insert(l.id, nid);
            }
            Op::Init => {
                let st_orig = l.args[0].unsigned_abs() as u32;
                if lec && imp_to_spec_state.contains_key(&st_orig) {
                    continue;
                }
                let srt = sort_imp[&l.sort.unwrap()];
                let st_new = map_i[&st_orig];
                let val_new = tr(l.args[1], &map_i, "imp")?;
                let nid = alloc();
                let _ = writeln!(out, "{nid} init {srt} {st_new} {val_new}");
            }
            Op::Next => {
                let st_orig = l.args[0].unsigned_abs() as u32;
                if lec && imp_to_spec_state.contains_key(&st_orig) {
                    continue;
                }
                let srt = sort_imp[&l.sort.unwrap()];
                let st_new = map_i[&st_orig];
                let val_new = tr(l.args[1], &map_i, "imp")?;
                pending_next.push((srt, st_new, val_new));
            }
            Op::Constraint => {
                if !lec
                    || !l
                        .symbol
                        .as_deref()
                        .is_some_and(|s| s.starts_with("svlang") && s.ends_with("_reset"))
                {
                    let c = tr(l.args[0], &map_i, "imp")?;
                    if !constraints.contains(&c) {
                        constraints.push(c);
                    }
                }
            }
            _ => {
                let srt = sort_imp[&l.sort.unwrap()];
                let args: Result<Vec<i64>, String> =
                    l.args.iter().map(|&a| tr(a, &map_i, "imp")).collect();
                let args = args?;
                let nid = alloc();
                let _ = write!(out, "{nid} {} {srt}", l.op.name());
                for a in &args {
                    let _ = write!(out, " {a}");
                }
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                map_i.insert(l.id, nid);
            }
        }
    }

    // 3. In SEC mode, tie matched uninitialized BitVec registers at step 0 (`first_step -> s0 == s1`).
    if !lec {
        let mut step0_ties = Vec::new();
        for &(s_st, i_st, _) in state_pairs {
            if !spec_init_states.contains(&s_st) && !imp_init_states.contains(&i_st) {
                let w_s = m_spec
                    .line(s_st)
                    .and_then(|l| l.sort)
                    .and_then(|sid| m_spec.width(sid));
                let w_i = m_imp
                    .line(i_st)
                    .and_then(|l| l.sort)
                    .and_then(|sid| m_imp.width(sid));
                if let (Some(ws), Some(wi)) = (w_s, w_i) {
                    if ws == wi {
                        step0_ties.push((map_s[&s_st], map_i[&i_st]));
                    }
                }
            }
        }
        if !step0_ties.is_empty() {
            let z1 = alloc();
            let _ = writeln!(out, "{z1} zero {sort1}");
            let o1 = alloc();
            let _ = writeln!(out, "{o1} one {sort1}");
            let fs = alloc();
            let _ = writeln!(out, "{fs} state {sort1} informal_sec_first_step");
            let fs_init = alloc();
            let _ = writeln!(out, "{fs_init} init {sort1} {fs} {o1}");
            pending_next.push((sort1, fs, z1 as i64));
            for (s0, s1) in step0_ties {
                let eq = alloc();
                let _ = writeln!(out, "{eq} eq {sort1} {s0} {s1}");
                let imp = alloc();
                let _ = writeln!(out, "{imp} implies {sort1} {fs} {eq}");
                constraints.push(imp as i64);
            }
        }
    }

    // 4. Translate `spec_ref` (optionally delayed by `target.latency`) and `imp_ref`.
    let mut spec_val = tr(target.spec_ref, &map_s, "spec")?;
    let imp_val = tr(target.imp_ref, &map_i, "imp")?;
    trace_outputs.push((spec_val, format!("spec.{}", target.spec_sig)));
    trace_outputs.push((imp_val, format!("imp.{}", target.imp_sig)));

    if !lec && target.latency > 0 {
        let z_tgt = alloc();
        let _ = writeln!(out, "{z_tgt} zero {sort_target}");
        for stage in 0..target.latency {
            let st = alloc();
            let _ = writeln!(
                out,
                "{st} state {sort_target} spec._sec_dly_{}_{stage}",
                sanitize_ident(&target.spec_sig)
            );
            let ini = alloc();
            let _ = writeln!(out, "{ini} init {sort_target} {st} {z_tgt}");
            pending_next.push((sort_target, st, spec_val));
            spec_val = st as i64;
        }
    }

    // 5. Post-reset + pipeline warmup counter (`skip_steps + target.latency` in SEC mode).
    let warmup = if lec { 0 } else { skip_steps + target.latency };
    let mut gate_ref: Option<i64> = None;
    if warmup > 0 {
        let z1 = alloc();
        let _ = writeln!(out, "{z1} zero {sort1}");
        let o1 = alloc();
        let _ = writeln!(out, "{o1} one {sort1}");
        let mut cur = o1 as i64;
        for step in 0..warmup {
            let st = alloc();
            let _ = writeln!(out, "{st} state {sort1} informal_sec_warmup_{step}");
            let ini = alloc();
            let _ = writeln!(out, "{ini} init {sort1} {st} {z1}");
            pending_next.push((sort1, st, cur));
            cur = st as i64;
        }
        gate_ref = Some(cur);
    }

    let diff = if spec_val == imp_val {
        let z1 = alloc();
        let _ = writeln!(out, "{z1} zero {sort1}");
        z1 as i64
    } else {
        let neq = alloc();
        let _ = writeln!(out, "{neq} neq {sort1} {spec_val} {imp_val}");
        neq as i64
    };
    let bad_cond = match gate_ref {
        None => diff,
        Some(g) => {
            let a = alloc();
            let _ = writeln!(out, "{a} and {sort1} {g} {diff}");
            a as i64
        }
    };

    // 6. Emit `next`, `constraint`, `bad`, and `; informal-trace-outputs`.
    for (srt, st, nxt) in pending_next {
        let nid = alloc();
        let _ = writeln!(out, "{nid} next {srt} {st} {nxt}");
    }
    for c in constraints {
        let nid = alloc();
        let _ = writeln!(out, "{nid} constraint {c}");
    }
    let bad_id = alloc();
    let _ = writeln!(out, "{bad_id} bad {bad_cond} {}", target.name);

    if !trace_outputs.is_empty() {
        let _ = writeln!(out, "{}", informal_svlang::TRACE_MARKER);
        let mut seen_out = HashSet::new();
        for (nref, name) in trace_outputs {
            if seen_out.insert(name.clone()) {
                let oid = alloc();
                let _ = writeln!(out, "{oid} output {nref} {name}");
            }
        }
    }

    Ok(out)
}

fn find_input_line(m: &Model, top: &str, name: &str) -> Option<(u32, u32, String)> {
    let rel_target = strip_top(name.trim(), Some(top));
    for l in m.with_op(Op::Input) {
        let Some(sym) = l.symbol.as_deref() else {
            continue;
        };
        let rel = strip_top(sym, Some(top));
        if rel == rel_target {
            let w = l.sort.and_then(|s| m.width(s))?;
            return Some((l.id, w, rel.to_string()));
        }
    }
    None
}

fn find_state_line(m: &Model, top: &str, name: &str) -> Option<(u32, Option<u32>, String)> {
    let rel_target = strip_top(name.trim(), Some(top));
    for l in m.with_op(Op::State) {
        let Some(sym) = l.symbol.as_deref() else {
            continue;
        };
        let rel = strip_top(sym, Some(top));
        if rel == rel_target {
            let w = l.sort.and_then(|s| m.width(s));
            return Some((l.id, w, rel.to_string()));
        }
    }
    None
}

/// Runs SEC or combinational LEC across `s.sec.setup`.
pub fn prove_sec(
    s: &mut Session,
    filter_name: Option<&str>,
    timeout_override: Option<Duration>,
) -> Result<(), String> {
    let Some(setup) = s.sec.setup.clone() else {
        return Err("check_sec -prove: run `check_sec -setup ...` first".into());
    };
    let spec_top = setup
        .spec_top
        .clone()
        .ok_or("check_sec -prove: missing spec top module")?;
    let imp_top = setup
        .imp_top
        .clone()
        .ok_or("check_sec -prove: missing imp top module")?;

    let clk_list: Vec<(String, u32, u32)> = match &setup.clock {
        Some(c) => vec![(c.clone(), 1, 0)],
        None => s
            .clocks
            .iter()
            .map(|c| (c.signal.clone(), c.factor, c.phase))
            .collect(),
    };
    let effective_reset = if setup.lec {
        None
    } else {
        setup.reset.clone().or_else(|| s.reset.clone())
    };
    let clk_sig = clk_list.first().map_or("clk", |c| c.0.as_str());
    let spec_props: Vec<(String, String)> = setup
        .assumptions
        .iter()
        .enumerate()
        .map(|(idx, expr)| {
            let trimmed = expr.trim();
            let full = if trimmed.starts_with("assume ") {
                trimmed.to_string()
            } else if trimmed.starts_with("@(") {
                format!("assume property ({trimmed})")
            } else {
                format!("assume property (@(posedge {clk_sig}) ({trimmed}))")
            };
            (format!("_sec_assume_{idx}"), full)
        })
        .collect();

    // Compile both for port discovery + lower both to word-level Btor2.
    let base_spec = informal_svlang::Request {
        files: setup.spec_files.clone(),
        defines: setup.spec_defines.clone(),
        incdirs: setup.spec_incdirs.clone(),
        top: Some(spec_top.clone()),
        params: setup.spec_params.clone(),
        trace_outputs: true,
        clocks: clk_list.clone(),
        properties: spec_props,
        reset: effective_reset.clone(),
        skip_initial_checks: s.design.skip_initial_checks,
        isunknown_is_false: s.design.isunknown_is_false,
        ..Default::default()
    };
    let spec_cd = informal_svlang::compile_cached(s.sec.spec_compiled.as_ref(), &base_spec)
        .map_err(|e| format!("check_sec (spec lower failed): {e}"))?;
    let l_spec = informal_svlang::lower_from_compiled(&spec_cd, &base_spec);
    if !l_spec.ok {
        return Err(format!(
            "check_sec (spec lower failed): {}",
            l_spec.errors.join("; ")
        ));
    }
    s.sec.spec_compiled = Some(Arc::clone(&spec_cd));
    let d_spec = informal_svlang::design_from_compiled(&spec_cd, &base_spec);

    let base_imp = informal_svlang::Request {
        files: setup.imp_files.clone(),
        defines: setup.imp_defines.clone(),
        incdirs: setup.imp_incdirs.clone(),
        top: Some(imp_top.clone()),
        params: setup.imp_params.clone(),
        trace_outputs: true,
        clocks: clk_list,
        reset: effective_reset.clone(),
        skip_initial_checks: s.design.skip_initial_checks,
        isunknown_is_false: s.design.isunknown_is_false,
        ..Default::default()
    };
    let imp_cd = informal_svlang::compile_cached(s.sec.imp_compiled.as_ref(), &base_imp)
        .map_err(|e| format!("check_sec (imp lower failed): {e}"))?;
    let l_imp = informal_svlang::lower_from_compiled(&imp_cd, &base_imp);
    if !l_imp.ok {
        return Err(format!(
            "check_sec (imp lower failed): {}",
            l_imp.errors.join("; ")
        ));
    }
    s.sec.imp_compiled = Some(Arc::clone(&imp_cd));
    let d_imp = informal_svlang::design_from_compiled(&imp_cd, &base_imp);

    let checks = prove_sec_from_btor(
        s,
        &setup,
        &spec_top,
        &imp_top,
        &d_spec,
        &d_imp,
        &l_spec.btor2,
        &l_imp.btor2,
        filter_name,
        timeout_override,
    )?;
    s.sec.checks = checks;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prove_sec_from_btor(
    s: &mut Session,
    setup: &SecSetup,
    spec_top: &str,
    imp_top: &str,
    d_spec: &informal_svlang::Design,
    d_imp: &informal_svlang::Design,
    spec_btor: &str,
    imp_btor: &str,
    filter_name: Option<&str>,
    timeout_override: Option<Duration>,
) -> Result<Vec<SecCheck>, String> {
    let effective_reset = if setup.lec {
        None
    } else {
        setup.reset.clone().or_else(|| s.reset.clone())
    };

    let m_spec = parse_full_model(spec_btor)?;
    let m_imp = parse_full_model(imp_btor)?;

    // 1. Pair primary inputs (`input_map` first, then matching relative names).
    let mut input_pairs: Vec<(u32, u32, String)> = Vec::new();
    let mut paired_spec_in: HashSet<u32> = HashSet::new();
    let mut paired_imp_in: HashSet<u32> = HashSet::new();

    for (s_name, i_name) in &setup.input_map {
        let (sid, sw, srel) = find_input_line(&m_spec, spec_top, s_name)
            .ok_or_else(|| format!("check_sec -map -input: `{s_name}` not found in spec"))?;
        let (iid, iw, _) = find_input_line(&m_imp, imp_top, i_name)
            .ok_or_else(|| format!("check_sec -map -input: `{i_name}` not found in imp"))?;
        if sw != iw {
            return Err(format!(
                "check_sec -map -input: width mismatch `{s_name}` ({sw} bits) vs `{i_name}` ({iw} bits)"
            ));
        }
        if paired_spec_in.insert(sid) && paired_imp_in.insert(iid) {
            input_pairs.push((sid, iid, srel));
        }
    }
    for l_s in m_spec.with_op(Op::Input) {
        if paired_spec_in.contains(&l_s.id) {
            continue;
        }
        let Some(sym_s) = l_s.symbol.as_deref() else {
            continue;
        };
        let rel_s = strip_top(sym_s, Some(spec_top));
        if is_internal_helper(rel_s) {
            continue;
        }
        if let Some((iid, iw, _)) = find_input_line(&m_imp, imp_top, rel_s) {
            if !paired_imp_in.contains(&iid) {
                let sw = l_s.sort.and_then(|st| m_spec.width(st)).unwrap_or(0);
                if sw != iw {
                    return Err(format!(
                        "check_sec: input `{rel_s}` has width {sw} in spec vs {iw} in imp"
                    ));
                }
                paired_spec_in.insert(l_s.id);
                paired_imp_in.insert(iid);
                input_pairs.push((l_s.id, iid, rel_s.to_string()));
            }
        }
    }

    // 2. Pair state registers (`state_map` first, then matching relative names).
    let mut state_pairs: Vec<(u32, u32, String)> = Vec::new();
    let mut paired_spec_st: HashSet<u32> = HashSet::new();
    let mut paired_imp_st: HashSet<u32> = HashSet::new();

    for (s_name, i_name) in &setup.state_map {
        let (sid, sw, srel) = find_state_line(&m_spec, spec_top, s_name)
            .ok_or_else(|| format!("check_sec -map -state: `{s_name}` not found in spec"))?;
        let (iid, iw, _) = find_state_line(&m_imp, imp_top, i_name)
            .ok_or_else(|| format!("check_sec -map -state: `{i_name}` not found in imp"))?;
        if sw != iw {
            return Err(format!(
                "check_sec -map -state: width mismatch `{s_name}` ({sw:?}) vs `{i_name}` ({iw:?})"
            ));
        }
        if paired_spec_st.insert(sid) && paired_imp_st.insert(iid) {
            state_pairs.push((sid, iid, srel));
        }
    }
    for l_s in m_spec.with_op(Op::State) {
        if paired_spec_st.contains(&l_s.id) {
            continue;
        }
        let Some(sym_s) = l_s.symbol.as_deref() else {
            continue;
        };
        let rel_s = strip_top(sym_s, Some(spec_top));
        if is_internal_helper(rel_s) {
            continue;
        }
        if let Some((iid, iw, _)) = find_state_line(&m_imp, imp_top, rel_s) {
            if !paired_imp_st.contains(&iid) {
                let sw = l_s.sort.and_then(|st| m_spec.width(st));
                if sw == iw {
                    paired_spec_st.insert(l_s.id);
                    paired_imp_st.insert(iid);
                    state_pairs.push((l_s.id, iid, rel_s.to_string()));
                }
            }
        }
    }

    // 3. Build equivalence targets (`lec_reg_*` + `lec_out_*` for LEC, `sec_out_*` for SEC).
    let mut targets: Vec<DualTarget> = Vec::new();
    if setup.lec {
        let spec_next: HashMap<u32, i64> = m_spec
            .with_op(Op::Next)
            .filter(|l| l.args.len() >= 2)
            .map(|l| (l.args[0].unsigned_abs() as u32, l.args[1]))
            .collect();
        let imp_next: HashMap<u32, i64> = m_imp
            .with_op(Op::Next)
            .filter(|l| l.args.len() >= 2)
            .map(|l| (l.args[0].unsigned_abs() as u32, l.args[1]))
            .collect();
        for &(sid, iid, ref rel) in &state_pairs {
            let Some(w) = m_spec
                .line(sid)
                .and_then(|l| l.sort)
                .and_then(|s| m_spec.width(s))
            else {
                continue;
            };
            if let (Some(&ns), Some(&ni)) = (spec_next.get(&sid), imp_next.get(&iid)) {
                targets.push(DualTarget {
                    name: format!("lec_reg_{}", sanitize_ident(rel)),
                    kind: "lec_state",
                    spec_sig: rel.clone(),
                    imp_sig: m_imp
                        .line(iid)
                        .and_then(|l| l.symbol.as_deref())
                        .map(|s| strip_top(s, Some(imp_top)).to_string())
                        .unwrap_or_else(|| rel.clone()),
                    spec_ref: ns,
                    imp_ref: ni,
                    width: w,
                    latency: 0,
                });
            }
        }
    }

    // Output pairs (`output_map` when explicit, otherwise all matched top-level outputs + explicit).
    let mut out_pairs: Vec<(String, String)> = setup.output_map.clone();
    let mapped_spec_outs: HashSet<String> = out_pairs.iter().map(|(a, _)| a.clone()).collect();
    let spec_outs = top_ports(d_spec, spec_top, true);
    let imp_outs: HashSet<String> = top_ports(d_imp, imp_top, true).into_iter().collect();
    for so in spec_outs {
        if !mapped_spec_outs.contains(&so) && imp_outs.contains(&so) {
            out_pairs.push((so.clone(), so));
        }
    }
    for (s_out, i_out) in out_pairs {
        let rs = resolve_signal(&m_spec, Some(spec_top), &s_out)
            .map_err(|e| format!("check_sec (spec output `{s_out}`): {e}"))?;
        let ri = resolve_signal(&m_imp, Some(imp_top), &i_out)
            .map_err(|e| format!("check_sec (imp output `{i_out}`): {e}"))?;
        if rs.width != ri.width {
            return Err(format!(
                "check_sec: output width mismatch `{s_out}` ({} bits) vs `{i_out}` ({} bits)",
                rs.width, ri.width
            ));
        }
        let s_rel = strip_top(&rs.name, Some(spec_top)).to_string();
        let i_rel = strip_top(&ri.name, Some(imp_top)).to_string();
        let lat = if setup.lec {
            0
        } else {
            setup
                .output_latency
                .get(&s_rel)
                .or_else(|| setup.output_latency.get(&i_rel))
                .copied()
                .unwrap_or(setup.latency)
        };
        let prefix = if setup.lec { "lec_out" } else { "sec_out" };
        let kind = if setup.lec {
            "lec_output"
        } else {
            "sec_output"
        };
        targets.push(DualTarget {
            name: format!("{prefix}_{}", sanitize_ident(&i_rel)),
            kind,
            spec_sig: s_rel,
            imp_sig: i_rel,
            spec_ref: rs.node_ref,
            imp_ref: ri.node_ref,
            width: rs.width,
            latency: lat,
        });
    }

    if let Some(pat) = filter_name {
        targets.retain(|t| crate::session::glob(pat, &t.name) || t.imp_sig == pat);
    }
    if targets.is_empty() {
        return Err("check_sec -prove: no matched outputs or state keypoints to compare".into());
    }

    let skip_steps = if effective_reset.is_some() { 1 } else { 0 };
    let timeout = timeout_override.unwrap_or(s.time_limit);
    let active_engines = s.active_engines();
    let explicit_filter = !s.engine_filter.is_empty();
    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("check_sec: {e}"))?;

    let mut checks = Vec::with_capacity(targets.len());
    for tgt in &targets {
        let full_miter = build_dual_miter_btor2(
            &m_spec,
            spec_top,
            &m_imp,
            imp_top,
            &input_pairs,
            &state_pairs,
            setup.lec,
            skip_steps,
            tgt,
        )?;
        let (engine_btor, _) = informal_svlang::split_trace(&full_miter);
        let btor_path = s.workdir.join(format!("{}.btor2", tgt.name));
        let trace_btor_path = s.workdir.join(format!("{}.trace.btor2", tgt.name));
        std::fs::write(&btor_path, engine_btor).map_err(|e| format!("check_sec: {e}"))?;
        std::fs::write(&trace_btor_path, &full_miter).map_err(|e| format!("check_sec: {e}"))?;

        let chosen = informal_engine::selector::select_for_btor(
            &active_engines,
            engine_btor,
            explicit_filter,
        );
        let req = CheckRequest {
            model: btor_path.clone(),
            format: ModelFormat::Btor2,
            timeout,
            effort: s.effort,
            max_depth: s.max_trace_length,
        };
        let run = portfolio::run_until(&chosen, &req, None, Some(s.stop.as_ref()));
        let eng = run.winner.as_ref().map(|w| w.engine.clone());
        let mut trace_path = None;
        let status = match portfolio::verdict(&run) {
            Verdict::Safe => {
                let _ = std::fs::remove_file(&btor_path);
                let _ = std::fs::remove_file(&trace_btor_path);
                SecStatus::Proven
            }
            Verdict::Unsafe { mut depth } => {
                let witness = run.winner.as_ref().and_then(|w| w.witness.as_deref());
                match s.write_trace(&tgt.name, &btor_path, witness, depth) {
                    Ok((path, d)) => {
                        depth.get_or_insert(d as u32);
                        trace_path = Some(path);
                        SecStatus::Cex { depth }
                    }
                    Err(Some(why)) => {
                        s.log(&format!(
                            "check_sec {}: counterexample did not replay ({why}); marked undetermined",
                            tgt.name
                        ));
                        SecStatus::Undetermined
                    }
                    Err(None) => SecStatus::Cex { depth },
                }
            }
            Verdict::Unknown { .. } => {
                let _ = std::fs::remove_file(&btor_path);
                let _ = std::fs::remove_file(&trace_btor_path);
                SecStatus::Undetermined
            }
        };
        checks.push(SecCheck {
            name: tgt.name.clone(),
            kind: tgt.kind.to_string(),
            spec_sig: tgt.spec_sig.clone(),
            imp_sig: tgt.imp_sig.clone(),
            width: tgt.width,
            latency: tgt.latency,
            status,
            engine: eng,
            ms: run.wall_ms,
            trace: trace_path,
        });
    }

    Ok(checks)
}

pub fn format_report(state: &SecState) -> String {
    let mut out = String::new();
    if let Some(ref setup) = state.setup {
        let mode = if setup.lec { "LEC" } else { "SEC" };
        let _ = writeln!(
            out,
            "mode: {mode}   spec: {}   imp: {}   latency: {}",
            setup.spec_top.as_deref().unwrap_or("-"),
            setup.imp_top.as_deref().unwrap_or("-"),
            setup.latency,
        );
    }
    let _ = writeln!(
        out,
        "{:<24} {:<12} {:<24} {:<14} {:<14} detail",
        "check", "kind", "spec <-> imp", "status", "engine"
    );
    let _ = writeln!(out, "{}", "-".repeat(96));
    for c in &state.checks {
        let pair = if c.spec_sig == c.imp_sig {
            format!("{} [{}b]", c.spec_sig, c.width)
        } else {
            format!("{} <-> {} [{}b]", c.spec_sig, c.imp_sig, c.width)
        };
        let detail = match &c.status {
            SecStatus::Proven => {
                if c.latency > 0 {
                    format!(
                        "{:.2} s (equivalent @ +{} cyc)",
                        c.ms as f64 / 1000.0,
                        c.latency
                    )
                } else {
                    format!("{:.2} s (equivalent)", c.ms as f64 / 1000.0)
                }
            }
            SecStatus::Cex { depth } => {
                let tr = c
                    .trace
                    .as_deref()
                    .map(|p| format!(" [trace: {p}]"))
                    .unwrap_or_default();
                format!(
                    "mismatch at depth {} in {:.2} s{tr}",
                    depth.map_or("?".into(), |d| d.to_string()),
                    c.ms as f64 / 1000.0
                )
            }
            SecStatus::Error(msg) => msg.clone(),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "{:<24} {:<12} {:<24} {:<14} {:<14} {}",
            c.name,
            c.kind,
            pair,
            c.status.word(),
            c.engine.as_deref().unwrap_or("-"),
            detail
        );
    }
    out
}
