//! Formal X-Propagation App (`check_xprop`).
//!
//! Uses exact 2-copy self-composition (`crate::miter`) rather than approximate
//! 3-valued ternary X-simulation: Copy 0 and Copy 1 share all deterministic
//! primary inputs, reset, and environment `assume` constraints, while
//! uninitialized registers (no `init`), explicit `X`/`Z` literals
//! (`informal_x_*`), undriven nets (`*_undriven`), and user-specified
//! `-x_source` signals take independent realizations in Copy 0 vs Copy 1.
//! Any sink (`outputs`, `control`, `memories`, `assertions`, or `-to <sig>`)
//! where `sink_0 != sink_1` after reset is a true X-propagation bug (`cex`);
//! `proven` certifies zero X-leakage across all reachable post-reset cycles.

use crate::miter::{
    build_miter_btor2, collect_xprop_sinks, parse_full_model, resolve_signal, MiterSpec,
};
use crate::session::{glob, Session};
use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_ir::Op;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static XPROP_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum XpropStatus {
    Unprocessed,
    Proven,
    Cex { depth: Option<u32> },
    Undetermined,
    Waived,
}

impl XpropStatus {
    pub fn word(self) -> &'static str {
        match self {
            Self::Unprocessed => "unprocessed",
            Self::Proven => "proven",
            Self::Cex { .. } => "cex",
            Self::Undetermined => "undetermined",
            Self::Waived => "waived",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct XpropWaiver {
    pub id: usize,
    pub pattern: String,
    pub source_only: bool,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XpropItem {
    pub name: String,
    pub category: String,
    pub status: XpropStatus,
    pub engine: Option<String>,
    pub ms: u128,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct XpropState {
    pub initialized: bool,
    pub categories: Vec<String>,
    pub x_sources: Vec<String>,
    pub explicit_targets: Vec<String>,
    pub waivers: Vec<XpropWaiver>,
    pub items: Vec<XpropItem>,
}

impl XpropState {
    pub fn reapply_sink_waivers(&mut self) {
        for it in &mut self.items {
            if it.status != XpropStatus::Proven
                && self
                    .waivers
                    .iter()
                    .filter(|w| !w.source_only)
                    .any(|w| glob(&w.pattern, &it.name) || it.name.contains(w.pattern.as_str()))
            {
                it.status = XpropStatus::Waived;
            }
        }
    }
}

pub fn prove(
    s: &mut Session,
    task: Option<&str>,
    timeout_override: Option<Duration>,
) -> Result<(), String> {
    if !s.elaborated {
        s.elaborate(None)
            .map_err(|e| format!("check_xprop: {e:#}"))?;
    }
    let task_name = match task {
        Some(t) => s.task_name(t)?,
        None => s.current_task.clone(),
    };
    let (full_btor, _) = s.lower_for_coverage(&task_name, false, false)?;
    let m = parse_full_model(&full_btor)?;
    let top = s.design.top.clone();

    // 1. Identify X-source inputs (`informal_x_*`, `*_undriven`, plus user `-x_source`).
    let mut diverging_inputs: HashSet<u32> = HashSet::new();
    let mut diverging_nodes: HashMap<u32, String> = HashMap::new();
    for l in m.lines().filter(|l| l.op == Op::Input) {
        if let Some(sym) = l.symbol.as_deref() {
            if sym.starts_with("informal_x_") || sym.ends_with("_undriven") {
                diverging_inputs.insert(l.id);
            }
        }
    }
    for src_name in &s.xprop.x_sources {
        let resolved = resolve_signal(&m, top.as_deref(), src_name)
            .map_err(|e| format!("check_xprop -x_source: {e}"))?;
        match resolved.direct_line {
            Some((id, Op::Input)) => {
                diverging_inputs.insert(id);
            }
            _ => {
                diverging_nodes.insert(resolved.node_ref.unsigned_abs() as u32, resolved.name);
            }
        }
    }

    // 2. Tie waived source flops (`-waiver -source`) or internal helper flops so they don't inject X.
    let mut tied_uninit_states: HashSet<u32> = HashSet::new();
    for l in m.lines().filter(|l| l.op == Op::State) {
        let Some(sym) = l.symbol.as_deref() else {
            continue;
        };
        let internal = sym.starts_with("informal_")
            || sym.contains("._sva_")
            || sym.contains("._past_")
            || sym.contains("._was_")
            || sym.contains("._clk_");
        let waived_src = s
            .xprop
            .waivers
            .iter()
            .filter(|w| w.source_only)
            .any(|w| glob(&w.pattern, sym) || sym.contains(w.pattern.as_str()));
        if internal || waived_src {
            tied_uninit_states.insert(l.id);
        }
    }

    // 3. Collect target sinks.
    let mut targets: Vec<(i64, u32, String, String)> = Vec::new();
    if !s.xprop.explicit_targets.is_empty() {
        for tname in &s.xprop.explicit_targets {
            let r = resolve_signal(&m, top.as_deref(), tname)
                .map_err(|e| format!("check_xprop -to: {e}"))?;
            targets.push((r.node_ref, r.width, r.name, "explicit".into()));
        }
    } else {
        let cats = if s.xprop.categories.is_empty() {
            vec![
                "outputs".to_string(),
                "control".to_string(),
                "memories".to_string(),
            ]
        } else {
            s.xprop.categories.clone()
        };
        let top_outputs: Vec<String> = s
            .design
            .hierarchy
            .iter()
            .find(|sc| Some(&sc.name) == top.as_ref() || Some(&sc.module) == top.as_ref())
            .or_else(|| s.design.hierarchy.first())
            .map(|sc| {
                sc.signals
                    .iter()
                    .filter(|sig| {
                        sig.kind == "port" && matches!(sig.dir.as_deref(), Some("output" | "inout"))
                    })
                    .map(|sig| sig.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        for (nref, w, name, cat) in collect_xprop_sinks(&m, top.as_deref(), &top_outputs, &cats) {
            targets.push((nref, w, name, cat.to_string()));
        }
    }

    let skip_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let timeout = timeout_override.unwrap_or(s.time_limit);
    let engines = informal_bmc::select_for_btor(
        &s.active_engines(),
        informal_svlang::split_trace(&full_btor).0,
        false,
    );
    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("check_xprop: {e}"))?;

    let mut items = Vec::with_capacity(targets.len());
    for (nref, w, name, cat) in targets {
        if s.xprop
            .waivers
            .iter()
            .filter(|wv| !wv.source_only)
            .any(|wv| glob(&wv.pattern, &name) || name.contains(wv.pattern.as_str()))
        {
            items.push(XpropItem {
                name,
                category: cat,
                status: XpropStatus::Waived,
                engine: None,
                ms: 0,
            });
            continue;
        }
        let spec = MiterSpec {
            diverging_inputs: diverging_inputs.clone(),
            diverging_nodes: diverging_nodes.clone(),
            uninit_states_diverge: true,
            tied_uninit_states: tied_uninit_states.clone(),
            skip_steps,
            guard_ref: None,
            target: (nref, w, format!("xprop_{name}")),
        };
        let miter_btor = build_miter_btor2(&m, &spec)?;
        let seq = XPROP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = s
            .workdir
            .join(format!("xprop_{}_{seq}.btor2", std::process::id()));
        std::fs::write(&path, &miter_btor).map_err(|e| format!("check_xprop: {e}"))?;
        let req = CheckRequest {
            model: path.clone(),
            format: ModelFormat::Btor2,
            timeout,
            effort: s.effort,
            max_depth: s.max_trace_length,
        };
        let run = portfolio::run_until(&engines, &req, None, Some(s.stop.as_ref()));
        let _ = std::fs::remove_file(&path);
        let eng = run.winner.as_ref().map(|w| w.engine.clone());
        let status = match portfolio::verdict(&run) {
            Verdict::Safe => XpropStatus::Proven,
            Verdict::Unsafe { depth } => XpropStatus::Cex { depth },
            Verdict::Unknown { .. } => XpropStatus::Undetermined,
        };
        items.push(XpropItem {
            name,
            category: cat,
            status,
            engine: eng,
            ms: run.wall_ms,
        });
    }

    s.xprop.items = items;
    s.xprop.initialized = true;
    Ok(())
}

pub fn format_report(state: &XpropState) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<32} {:<10} {:<14} {:<14} detail",
        "target", "category", "status", "engine"
    );
    let _ = writeln!(out, "{}", "-".repeat(78));
    for it in &state.items {
        let detail = match it.status {
            XpropStatus::Proven => format!("{:.2} s (X-clean)", it.ms as f64 / 1000.0),
            XpropStatus::Cex { depth } => format!(
                "X-leakage at depth {} in {:.2} s",
                depth.map_or("?".into(), |d| d.to_string()),
                it.ms as f64 / 1000.0
            ),
            XpropStatus::Waived => "waived".into(),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "{:<32} {:<10} {:<14} {:<14} {}",
            it.name,
            it.category,
            it.status.word(),
            it.engine.as_deref().unwrap_or("-"),
            detail
        );
    }
    out
}
