//! Formal Coverage & UNR App (`check_cov`).
//!
//! Provides three complementary formal sign-off metrics over the lowered design:
//! - **Formal COI Observability (`coi` / `statement`)**: exact backward
//!   cone-of-influence traversal over `informal_ir::Model` from enabled (or
//!   `Proven`, with `-enable_prove_based_proof_core`) assertion targets to
//!   registers (`state`) and inputs (`input`), flagging RTL logic no assertion
//!   observes.
//! - **Dead-Code & Stuck-Bit UNR (`branch` / `toggle`)**: compiler-synthesized
//!   mux/branch true-false covers and register rise/fall toggle covers checked
//!   under the task's reset and assumptions, classifying unreachable branches
//!   and stuck state bits as `UNR`.
//! - **Functional Covers (`functional`)**: status summary of user and RTL
//!   `cover` properties, unified with structured `check_cov -waiver` waivers.

use crate::session::{glob, only_target, target_name, Kind, Session, Status};
use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_ir::Op;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static COV_TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CovModel {
    Coi,
    Branch,
    Toggle,
    Functional,
}

impl CovModel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coi => "coi",
            Self::Branch => "branch",
            Self::Toggle => "toggle",
            Self::Functional => "functional",
        }
    }

    pub fn parse_list(spec: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        for tok in spec.split(|c: char| c.is_whitespace() || c == ',') {
            if tok.is_empty() {
                continue;
            }
            let mapped: &[Self] = match tok.to_ascii_lowercase().as_str() {
                "all" => &[Self::Coi, Self::Branch, Self::Toggle, Self::Functional],
                "coi" | "observability" | "statement" | "stimuli" | "checker" => &[Self::Coi],
                "branch" | "expression" | "cond" => &[Self::Branch],
                "toggle" => &[Self::Toggle],
                "functional" | "cover" => &[Self::Functional],
                other => {
                    return Err(format!(
                        "check_cov: unknown coverage type `{other}` (expected coi, branch, toggle, functional, statement, or all)"
                    ))
                }
            };
            for &m in mapped {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CovStatus {
    Covered,
    Uncovered,
    Unreachable,
    Undetermined,
    Waived,
}

impl CovStatus {
    pub fn word(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::Uncovered => "uncovered",
            Self::Unreachable => "unreachable (UNR)",
            Self::Undetermined => "undetermined",
            Self::Waived => "waived",
        }
    }

    pub fn matches_exclude(self, ex: &str) -> bool {
        match ex.to_ascii_lowercase().as_str() {
            "covered" => self == Self::Covered,
            "uncovered" => self == Self::Uncovered,
            "unreachable" | "unr" => self == Self::Unreachable,
            "undetermined" | "unprocessed" => self == Self::Undetermined,
            "waived" => self == Self::Waived,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CovWaiver {
    pub id: usize,
    pub models: Vec<CovModel>,
    pub instance: Option<String>,
    pub source_file: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub expression: Option<String>,
    pub cover_item_id: Option<String>,
    pub comment: Option<String>,
}

impl CovWaiver {
    pub fn matches(&self, item: &CovItem) -> bool {
        if !self.models.is_empty() && !self.models.contains(&item.model) {
            return false;
        }
        if let Some(cid) = &self.cover_item_id {
            if !glob(cid, &item.id) && !glob(cid, &item.name) {
                return false;
            }
        }
        if let Some(inst) = &self.instance {
            let pat = if inst.contains('*') || inst.contains('?') {
                inst.clone()
            } else {
                format!("*{inst}*")
            };
            if !glob(&pat, &item.name) {
                return false;
            }
        }
        if let Some(expr) = &self.expression {
            let pat = if expr.contains('*') || expr.contains('?') {
                expr.clone()
            } else {
                format!("*{expr}*")
            };
            if !glob(&pat, &item.detail) && !glob(&pat, &item.name) {
                return false;
            }
        }
        if let Some(sf) = &self.source_file {
            let Some(item_file) = &item.file else {
                return false;
            };
            if !item_file.ends_with(sf.as_str()) && !glob(sf, item_file) {
                return false;
            }
        }
        if self.start_line.is_some() || self.end_line.is_some() {
            let Some(line) = item.line else {
                return false;
            };
            let lo = self.start_line.unwrap_or(0);
            let hi = self.end_line.unwrap_or(u32::MAX);
            if line < lo || line > hi {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CovItem {
    pub id: String,
    pub task: String,
    pub model: CovModel,
    pub name: String,
    pub status: CovStatus,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CovState {
    pub initialized: bool,
    pub models: Vec<CovModel>,
    pub prove_based: bool,
    pub waivers: Vec<CovWaiver>,
    pub items: Vec<CovItem>,
    pub measured: bool,
}

impl CovState {
    pub fn active_models(&self) -> Vec<CovModel> {
        if self.models.is_empty() {
            vec![
                CovModel::Coi,
                CovModel::Branch,
                CovModel::Toggle,
                CovModel::Functional,
            ]
        } else {
            self.models.clone()
        }
    }

    pub fn reapply_waivers(&mut self) {
        for item in &mut self.items {
            if item.status != CovStatus::Covered && self.waivers.iter().any(|w| w.matches(item)) {
                item.status = CovStatus::Waived;
            }
        }
    }
}

pub fn measure(
    s: &mut Session,
    tasks: &[String],
    models: &[CovModel],
    timeout_override: Option<Duration>,
) -> Result<(), String> {
    if !s.elaborated {
        s.elaborate(None).map_err(|e| format!("check_cov: {e:#}"))?;
    }
    let tasks: Vec<String> = if tasks.is_empty() {
        vec![s.current_task.clone()]
    } else {
        let mut out = Vec::with_capacity(tasks.len());
        for t in tasks {
            out.push(s.task_name(t)?);
        }
        out
    };
    let models = if models.is_empty() {
        s.cov.active_models()
    } else {
        models.to_vec()
    };

    let want_coi = models.contains(&CovModel::Coi);
    let want_branch = models.contains(&CovModel::Branch);
    let want_toggle = models.contains(&CovModel::Toggle);
    let want_func = models.contains(&CovModel::Functional);
    let per_item_timeout =
        timeout_override.unwrap_or_else(|| s.time_limit.min(Duration::from_secs(5)));

    let mut items: Vec<CovItem> = Vec::new();
    let mut seq = 1usize;

    for task in &tasks {
        if want_coi {
            let coi_items = measure_task_coi(s, task, s.cov.prove_based, &mut seq)?;
            items.extend(coi_items);
        }
        if want_branch || want_toggle {
            let unr_items = measure_task_unr(
                s,
                task,
                want_branch,
                want_toggle,
                per_item_timeout,
                &mut seq,
            )?;
            items.extend(unr_items);
        }
        if want_func {
            for p in s
                .props
                .iter()
                .filter(|p| &p.task == task && p.kind == Kind::Cover && p.enabled)
            {
                let status = match p.status {
                    Status::Covered { .. } => CovStatus::Covered,
                    Status::Unreachable { .. } => CovStatus::Unreachable,
                    _ => CovStatus::Undetermined,
                };
                items.push(CovItem {
                    id: format!("COV_{seq:04}"),
                    task: task.clone(),
                    model: CovModel::Functional,
                    name: p.name.clone(),
                    status,
                    file: p.source.as_ref().map(|l| l.file.clone()),
                    line: p.source.as_ref().map(|l| l.line),
                    detail: p.expr.clone(),
                });
                seq += 1;
            }
        }
    }

    s.cov.items = items;
    s.cov.measured = true;
    s.cov.reapply_waivers();
    Ok(())
}

fn measure_task_coi(
    s: &Session,
    task: &str,
    prove_based_only: bool,
    seq: &mut usize,
) -> Result<Vec<CovItem>, String> {
    let btor = s.task_model(task)?;
    let m = informal_ir::parse(&btor).map_err(|e| format!("check_cov: {e}"))?;

    let has_proven = s.props.iter().any(|p| {
        p.task == task
            && p.kind == Kind::Assert
            && p.enabled
            && matches!(p.status, Status::Proven { .. })
    });
    let use_proven_only = prove_based_only || has_proven;

    let mut active_targets: HashSet<String> = HashSet::new();
    for (idx, p) in s.props.iter().enumerate() {
        if p.task != task || p.kind != Kind::Assert || !p.enabled {
            continue;
        }
        if use_proven_only && !matches!(p.status, Status::Proven { .. }) {
            continue;
        }
        active_targets.insert(s.check_name(idx));
    }

    let mut next_deps: HashMap<u32, Vec<u32>> = HashMap::new();
    for l in m.lines().filter(|l| matches!(l.op, Op::Next | Op::Init)) {
        if let [state, rest @ ..] = l.args.as_slice() {
            next_deps
                .entry(state.unsigned_abs() as u32)
                .or_default()
                .extend(rest.iter().map(|a| a.unsigned_abs() as u32));
        }
    }

    let mut todo: Vec<u32> = Vec::new();
    for l in m.lines() {
        if matches!(l.op, Op::Bad | Op::Justice) {
            if let Some(sym) = l.symbol.as_deref() {
                if active_targets.contains(sym) {
                    todo.extend(l.args.iter().map(|a| a.unsigned_abs() as u32));
                }
            }
        }
    }

    let mut seen: HashSet<u32> = HashSet::new();
    let mut covered_syms: HashSet<String> = HashSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(l) = m.line(id) else { continue };
        match l.op {
            Op::State => {
                if let Some(sym) = &l.symbol {
                    covered_syms.insert(sym.clone());
                }
                todo.extend(next_deps.get(&id).into_iter().flatten().copied());
            }
            Op::Input => {
                if let Some(sym) = &l.symbol {
                    covered_syms.insert(sym.clone());
                }
            }
            _ => todo.extend(l.args.iter().map(|a| a.unsigned_abs() as u32)),
        }
    }

    let top_prefix = s
        .design
        .top
        .as_deref()
        .map(|t| format!("{t}."))
        .unwrap_or_default();
    let clock_names: HashSet<String> = s
        .clocks
        .iter()
        .flat_map(|c| [c.signal.clone(), format!("{top_prefix}{}", c.signal)])
        .collect();

    let mut out = Vec::new();
    for l in m.lines() {
        if !matches!(l.op, Op::State | Op::Input) {
            continue;
        }
        let Some(sym) = l.symbol.as_deref() else {
            continue;
        };
        if sym.starts_with("informal_")
            || sym.contains("_sva_")
            || sym.contains("_past_")
            || sym.contains("_was_")
            || sym.contains("_clk_")
            || clock_names.contains(sym)
        {
            continue;
        }
        let kind_word = if l.op == Op::State {
            "register"
        } else {
            "input"
        };
        let status = if covered_syms.contains(sym) {
            CovStatus::Covered
        } else {
            CovStatus::Uncovered
        };
        out.push(CovItem {
            id: format!("COV_{seq:04}"),
            task: task.to_string(),
            model: CovModel::Coi,
            name: sym.to_string(),
            status,
            file: None,
            line: None,
            detail: format!("COI {kind_word}"),
        });
        *seq += 1;
    }
    Ok(out)
}

fn measure_task_unr(
    s: &Session,
    task: &str,
    want_branch: bool,
    want_toggle: bool,
    timeout: Duration,
    seq: &mut usize,
) -> Result<Vec<CovItem>, String> {
    let (btor, props) = s.lower_for_coverage(task, want_branch, want_toggle)?;
    let (engine_btor, _) = informal_svlang::split_trace(&btor);
    let safety_engines: Vec<_> = s
        .active_engines()
        .into_iter()
        .filter(|e| e.name() != "informal-klive")
        .collect();
    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("check_cov: {e}"))?;

    let mut out = Vec::new();
    for p in props {
        let model_kind = if p.name.contains("._cov_branch_") {
            if !want_branch {
                continue;
            }
            CovModel::Branch
        } else if p.name.contains("._cov_toggle") {
            if !want_toggle {
                continue;
            }
            CovModel::Toggle
        } else {
            continue;
        };
        let keep = target_name(&p.name);
        let isolated = only_target(engine_btor, &keep);
        let has_bad = isolated
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some("bad"));
        let status = if !has_bad {
            CovStatus::Unreachable
        } else {
            let tmp_id = COV_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let model_path = s
                .workdir
                .join(format!("cov_{}_{tmp_id}.btor2", std::process::id()));
            std::fs::write(&model_path, &isolated).map_err(|e| format!("check_cov: {e}"))?;
            let req = CheckRequest {
                model: model_path.clone(),
                format: ModelFormat::Btor2,
                timeout,
            };
            let run = portfolio::run_until(&safety_engines, &req, None, Some(s.stop.as_ref()));
            let _ = std::fs::remove_file(&model_path);
            match portfolio::verdict(&run) {
                Verdict::Unsafe { .. } => CovStatus::Covered,
                Verdict::Safe => CovStatus::Unreachable,
                Verdict::Unknown { .. } => CovStatus::Undetermined,
            }
        };
        let detail = match model_kind {
            CovModel::Branch => {
                if p.name.ends_with("_true") {
                    "branch true arm".into()
                } else {
                    "branch false arm".into()
                }
            }
            CovModel::Toggle => {
                if p.name.ends_with("_rise") {
                    "toggle 0->1".into()
                } else if p.name.ends_with("_fall") {
                    "toggle 1->0".into()
                } else {
                    "toggle change".into()
                }
            }
            _ => String::new(),
        };
        out.push(CovItem {
            id: format!("COV_{seq:04}"),
            task: task.to_string(),
            model: model_kind,
            name: p.name,
            status,
            file: p.file,
            line: p.line,
            detail,
        });
        *seq += 1;
    }
    Ok(out)
}

pub fn filter_items<'a>(
    cov: &'a CovState,
    models: Option<&[CovModel]>,
    tasks: Option<&[String]>,
    excludes: &[String],
) -> Vec<&'a CovItem> {
    cov.items
        .iter()
        .filter(|it| models.is_none_or(|ms| ms.contains(&it.model)))
        .filter(|it| tasks.is_none_or(|ts| ts.is_empty() || ts.contains(&it.task)))
        .filter(|it| !excludes.iter().any(|ex| it.status.matches_exclude(ex)))
        .collect()
}

pub fn format_report(
    cov: &CovState,
    models: Option<&[CovModel]>,
    tasks: Option<&[String]>,
    excludes: &[String],
    html: bool,
) -> String {
    let shown = filter_items(cov, models, tasks, excludes);
    if html {
        return format_html_report(&shown, &cov.waivers);
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<12} {:<8} {:<8} {:<10} {:<8} {:<8} {:<8}",
        "model", "total", "covered", "uncov/UNR", "waived", "undet", "score"
    );
    let _ = writeln!(out, "{}", "-".repeat(68));
    for m in [
        CovModel::Coi,
        CovModel::Branch,
        CovModel::Toggle,
        CovModel::Functional,
    ] {
        if models.is_some_and(|ms| !ms.contains(&m)) {
            continue;
        }
        let group: Vec<&&CovItem> = shown.iter().filter(|it| it.model == m).collect();
        if group.is_empty() {
            continue;
        }
        let total = group.len();
        let covered = group
            .iter()
            .filter(|it| it.status == CovStatus::Covered)
            .count();
        let waived = group
            .iter()
            .filter(|it| it.status == CovStatus::Waived)
            .count();
        let bad = group
            .iter()
            .filter(|it| matches!(it.status, CovStatus::Uncovered | CovStatus::Unreachable))
            .count();
        let undet = group
            .iter()
            .filter(|it| it.status == CovStatus::Undetermined)
            .count();
        let denom = total.saturating_sub(waived);
        let pct = if denom == 0 {
            100.0
        } else {
            (covered as f64) * 100.0 / (denom as f64)
        };
        let _ = writeln!(
            out,
            "{:<12} {:<8} {:<8} {:<10} {:<8} {:<8} {:>6.1}%",
            m.as_str(),
            total,
            covered,
            bad,
            waived,
            undet,
            pct
        );
    }

    if !shown.is_empty() {
        let _ = writeln!(
            out,
            "\n{:<10} {:<11} {:<18} {:<32} detail",
            "id", "model", "status", "item"
        );
        for it in &shown {
            let loc = match (&it.file, it.line) {
                (Some(f), Some(l)) => {
                    let short = std::path::Path::new(f)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(f);
                    format!(" ({short}:{l})")
                }
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "{:<10} {:<11} {:<18} {:<32} {}{}",
                it.id,
                it.model.as_str(),
                it.status.word(),
                it.name,
                it.detail,
                loc
            );
        }
    }
    out
}

fn format_html_report(items: &[&CovItem], waivers: &[CovWaiver]) -> String {
    let mut html = String::from(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>InFormal Coverage Report</title>\
         <style>body{font-family:sans-serif;margin:24px}table{border-collapse:collapse;width:100%}\
         th,td{border:1px solid #ccc;padding:6px 10px;text-align:left}</style></head><body>\
         <h1>InFormal Formal Coverage &amp; UNR Report</h1><table>\
         <tr><th>ID</th><th>Task</th><th>Model</th><th>Status</th><th>Item</th><th>Detail</th></tr>\n",
    );
    for it in items {
        let _ = writeln!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            it.id,
            it.task,
            it.model.as_str(),
            it.status.word(),
            it.name,
            it.detail
        );
    }
    let _ = writeln!(
        html,
        "</table><p>Active waivers: {}</p></body></html>",
        waivers.len()
    );
    html
}
