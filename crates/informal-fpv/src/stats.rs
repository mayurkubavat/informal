//! Design and proof statistics (`get_design_info`, `report -design`,
//! `get_property_info`, `report -detail`): what the design and its model
//! hold, and what each prove measured.
//!
//! Every number here is counted or measured; none is estimated. A value
//! that could not be measured is left out (`None`), and the readable
//! tables say "not measured" or "not checked" in its place.

use crate::session::{Kind, Property, Session, Status, Vacuity};
use informal_engine::portfolio::{EngineRun, RunOutcome};
use informal_engine::Certificate;
use informal_ir::{Model, Op, Sort};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// What a model (Btor2, as the engines get it) holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModelStats {
    /// Btor2 lines other than sorts.
    pub nodes: usize,
    pub states: usize,
    pub state_bits: u64,
    /// States that are arrays (memories larger than the front end turns
    /// into one register per word).
    pub array_states: usize,
    pub inputs: usize,
    pub input_bits: u64,
    /// `constraint` lines: assumptions, and the reset.
    pub constraints: usize,
    /// Targets: `bad` (assertions, covers, precondition covers) and
    /// `justice` (liveness) lines.
    pub bad: usize,
    pub justice: usize,
    /// The model bit-blasted to an and-inverter graph, as the in-house
    /// engines load it; `None` when it does not bit-blast (arrays, or an
    /// error: see `aig_error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aig: Option<AigStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aig_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AigStats {
    pub inputs: usize,
    pub latches: usize,
    pub ands: usize,
}

/// One property's cone of influence in its model: the states and inputs
/// its target and the constraints depend on, through any number of steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Cone {
    pub states: usize,
    pub state_bits: u64,
    pub inputs: usize,
    pub input_bits: u64,
}

/// `get_design_info`: the design as elaborated, and the model of the
/// current task after lowering.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DesignInfo {
    pub top: Option<String>,
    pub files: usize,
    pub elaborated: bool,
    /// Counted on the netlist as elaborated: black boxes and cut points
    /// are not applied yet. `None` before elaboration or when the design
    /// does not lower.
    pub design: Option<informal_svlang::DesignStats>,
    /// Clocks and reset as the script declared them.
    pub declared_clocks: Vec<String>,
    pub declared_reset: Option<String>,
    pub black_boxes: Vec<String>,
    /// The task the next three and `model` are of.
    pub task: String,
    pub cut_points: Vec<String>,
    pub properties: PropCounts,
    /// The current task's model as the engines get it. `None` when it was
    /// not built (not elaborated) or did not lower (`model_error`).
    pub model: Option<ModelStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PropCounts {
    pub asserts: usize,
    pub covers: usize,
    /// Enabled assumptions (constraints) of the task, those of the design
    /// included.
    pub assumptions: usize,
    pub disabled: usize,
}

/// What a prove measured for one property. Only what an engine or the
/// session measured is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProofStats {
    /// The engine whose verdict was taken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Wall time of the portfolio run on this property.
    pub wall_ms: u64,
    /// CPU time (user and system) of the whole process and of the engine
    /// processes it waited for, while this property's portfolio ran. The
    /// engines run in parallel, so it can exceed the wall time. `None`
    /// where the platform does not report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
    /// Peak resident memory reached while the property ran, in KiB: set
    /// only when the process's (or an engine process's) high-water mark
    /// rose during the run, so it is this run's peak; `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_kb: Option<u64>,
    /// The depth that settled it, and what it counts: `trace` (cycles to
    /// the failure or the cover), `k` (k-induction), `frames` (IC3/PDR
    /// frames when the invariant was found), or `bound` (BMC bound
    /// reached, for an undetermined result).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth_kind: Option<&'static str>,
    /// The deepest bound any engine reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bound: Option<u32>,
    /// Clauses of the inductive invariant (lemmas), when the winner gave
    /// one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invariant_clauses: Option<usize>,
    /// The kind of evidence the winner gave (`invariant`, `k-induction`,
    /// `two-step`, `two-step-k-induction`, `trace`); `None` when it gave
    /// none (external engines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// The certificate written for an outside checker, or why none was
    /// (`informal shell --certificate`); `None` when none was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
}

impl ProofStats {
    /// From a finished portfolio run.
    pub fn of_run(
        status: &Status,
        runs: &[EngineRun],
        wall_ms: u128,
        winner_certificate: Option<&Certificate>,
        usage: Option<(Usage, Usage)>,
    ) -> ProofStats {
        let engine = match status {
            Status::Proven { engine, .. }
            | Status::Unreachable { engine, .. }
            | Status::Cex { engine, .. }
            | Status::Covered { engine, .. } => Some(engine.clone()),
            _ => None,
        };
        let won = runs.iter().find(|r| r.outcome == RunOutcome::Won);
        let max_bound = runs.iter().filter_map(|r| r.depth).max();
        let clauses = winner_certificate.and_then(|c| match c {
            Certificate::Invariant { clauses }
            | Certificate::TwoStep { clauses }
            | Certificate::KInduction { clauses, .. }
            | Certificate::TwoStepKInduction { clauses, .. } => Some(clauses.len()),
            Certificate::Trace { .. } => None,
        });
        let (depth, depth_kind) = match (status, winner_certificate) {
            (Status::Cex { depth, .. } | Status::Covered { depth, .. }, _) => {
                (*depth, depth.map(|_| "trace"))
            }
            (
                Status::Proven { .. } | Status::Unreachable { .. },
                Some(Certificate::KInduction { k, .. } | Certificate::TwoStepKInduction { k, .. }),
            ) => (Some(*k), Some("k")),
            (
                Status::Proven { .. } | Status::Unreachable { .. },
                Some(Certificate::Invariant { .. } | Certificate::TwoStep { .. }),
            ) => {
                let d = won.and_then(|r| r.depth);
                (d, d.map(|_| "frames"))
            }
            (Status::Undetermined { .. }, _) => (max_bound, max_bound.map(|_| "bound")),
            _ => (None, None),
        };
        let (cpu_ms, peak_rss_kb) = match usage {
            Some((a, b)) => (
                Some(b.cpu_ms.saturating_sub(a.cpu_ms)),
                (b.peak_rss_kb > a.peak_rss_kb).then_some(b.peak_rss_kb),
            ),
            None => (None, None),
        };
        ProofStats {
            engine,
            wall_ms: wall_ms as u64,
            cpu_ms,
            peak_rss_kb,
            depth,
            depth_kind,
            max_bound,
            invariant_clauses: clauses,
            evidence: winner_certificate.map(|c| c.kind().to_string()),
            certificate: None,
        }
    }
}

/// The process's resource use so far: CPU time of this process and of
/// the child processes it has waited for, and the larger of the two
/// high-water marks of resident memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub cpu_ms: u64,
    pub peak_rss_kb: u64,
}

impl Usage {
    /// `None` where `getrusage` is not available.
    pub fn now() -> Option<Usage> {
        #[cfg(unix)]
        {
            let get = |who| {
                // SAFETY: getrusage fills the struct it is given.
                let mut r: libc::rusage = unsafe { std::mem::zeroed() };
                (unsafe { libc::getrusage(who, &mut r) } == 0).then_some(r)
            };
            let me = get(libc::RUSAGE_SELF)?;
            let kids = get(libc::RUSAGE_CHILDREN)?;
            let ms = |t: libc::timeval| t.tv_sec as u64 * 1000 + t.tv_usec as u64 / 1000;
            // Linux reports ru_maxrss in KiB, macOS in bytes.
            let kb = |v: libc::c_long| {
                let v = v.max(0) as u64;
                if cfg!(target_os = "macos") {
                    v / 1024
                } else {
                    v
                }
            };
            Some(Usage {
                cpu_ms: ms(me.ru_utime) + ms(me.ru_stime) + ms(kids.ru_utime) + ms(kids.ru_stime),
                peak_rss_kb: kb(me.ru_maxrss).max(kb(kids.ru_maxrss)),
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Count what a Btor2 model holds; with `aig`, also bit-blast it as the
/// in-house engines would.
pub fn model_stats(btor: &str, aig: bool) -> Result<ModelStats, String> {
    let m = informal_ir::parse(btor).map_err(|e| e.to_string())?;
    let f = informal_ir::Features::of(&m);
    let input_bits = m
        .with_op(Op::Input)
        .filter_map(|l| l.sort.and_then(|s| m.width(s)))
        .map(u64::from)
        .sum();
    let mut st = ModelStats {
        nodes: f.nodes,
        states: f.states,
        state_bits: f.state_bits,
        array_states: f.array_states,
        inputs: f.inputs,
        input_bits,
        constraints: f.constraints,
        bad: f.bad,
        justice: f.justice,
        aig: None,
        aig_error: None,
    };
    if aig {
        match informal_aig::btor::from_btor2(&m) {
            Ok(a) => {
                st.aig = Some(AigStats {
                    inputs: a.inputs.len(),
                    latches: a.latches.len(),
                    ands: a.num_ands(),
                })
            }
            Err(e) => st.aig_error = Some(format!("{e:#}")),
        }
    }
    Ok(st)
}

/// The cone of influence of the target named `target` (a `bad` or
/// `justice` line) in `btor`, with every constraint's cone: what an
/// engine has to reason about for it.
pub fn cone(btor: &str, target: &str) -> Result<Cone, String> {
    let m = informal_ir::parse(btor).map_err(|e| e.to_string())?;
    let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
    for l in m.lines().filter(|l| matches!(l.op, Op::Next | Op::Init)) {
        if let [state, rest @ ..] = l.args.as_slice() {
            next.entry(state.unsigned_abs() as u32)
                .or_default()
                .extend(rest.iter().map(|a| a.unsigned_abs() as u32));
        }
    }
    let mut todo: Vec<u32> = Vec::new();
    let mut found = false;
    for l in m.lines() {
        let named = l.symbol.as_deref() == Some(target);
        match l.op {
            Op::Bad | Op::Justice if named => {
                found = true;
                todo.extend(l.args.iter().map(|a| a.unsigned_abs() as u32));
            }
            Op::Constraint => todo.extend(l.args.iter().map(|a| a.unsigned_abs() as u32)),
            _ => {}
        }
    }
    if !found {
        return Err(format!("no target named {target} in the model"));
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut c = Cone::default();
    let width = |m: &Model, l: &informal_ir::Line| -> u64 {
        match l.sort.and_then(|s| m.sort(s)) {
            Some(Sort::BitVec(w)) => u64::from(w),
            _ => 0,
        }
    };
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(l) = m.line(id) else { continue };
        match l.op {
            Op::State => {
                c.states += 1;
                c.state_bits += width(&m, l);
                todo.extend(next.get(&id).into_iter().flatten().copied());
            }
            Op::Input => {
                c.inputs += 1;
                c.input_bits += width(&m, l);
            }
            _ => todo.extend(l.args.iter().map(|a| a.unsigned_abs() as u32)),
        }
    }
    Ok(c)
}

impl Session {
    /// `get_design_info`: the design's counts and, when elaborated, the
    /// current task's model after lowering (the model is built for it).
    pub fn design_info(&mut self) -> DesignInfo {
        let task = self.current_task.clone();
        let of_task = |p: &&Property| p.task == task;
        let props = &self.props;
        let mut info = DesignInfo {
            top: self.design.top.clone(),
            files: self.design.files.len(),
            elaborated: self.elaborated,
            design: self.elab.as_ref().and_then(|d| d.stats.clone()),
            declared_clocks: self
                .clocks
                .iter()
                .map(|c| match (c.factor, c.phase) {
                    (1, 0) => c.signal.clone(),
                    (f, p) => format!("{} (factor {f}, phase {p})", c.signal),
                })
                .collect(),
            declared_reset: self.reset.clone(),
            black_boxes: self
                .design
                .bbox
                .iter()
                .chain(&self.design.bbox_instances)
                .cloned()
                .collect(),
            cut_points: self.stopats_of(&task).to_vec(),
            properties: PropCounts {
                asserts: props
                    .iter()
                    .filter(of_task)
                    .filter(|p| p.kind == Kind::Assert)
                    .count(),
                covers: props
                    .iter()
                    .filter(of_task)
                    .filter(|p| p.kind == Kind::Cover)
                    .count(),
                assumptions: props
                    .iter()
                    .filter(of_task)
                    .filter(|p| p.kind == Kind::Assume && p.enabled)
                    .count(),
                disabled: props.iter().filter(of_task).filter(|p| !p.enabled).count(),
            },
            task: task.clone(),
            model: None,
            model_error: None,
        };
        if !self.elaborated || self.dry_run {
            return info;
        }
        match self.task_model(&task) {
            Ok(btor) => match model_stats(&btor, true) {
                Ok(m) => info.model = Some(m),
                Err(e) => info.model_error = Some(e),
            },
            Err(e) => info.model_error = Some(e),
        }
        info
    }

    /// `get_property_info`: everything known about property `i`, with its
    /// cone of influence when `cone` is asked for (it lowers the design).
    pub fn property_info(&self, i: usize, with_cone: bool) -> PropertyInfo {
        let p = &self.props[i];
        let cone =
            (with_cone && p.kind != Kind::Assume && self.elaborated && !self.dry_run).then(|| {
                self.model_for(i)
                    .and_then(|m| cone(&m, &self.check_name(i)))
            });
        PropertyInfo {
            name: p.name.clone(),
            kind: p.kind.label(),
            task: p.task.clone(),
            enabled: p.enabled,
            status: p.status.word(),
            expr: p.expr.clone(),
            vacuity: match (&p.status, p.vacuity) {
                (_, Some(v)) => v.word(),
                (Status::Proven { .. }, None) if p.kind == Kind::Assert => "not checked",
                _ => "",
            },
            stats: p.stats.clone(),
            engines: p.engines.clone(),
            cone: cone.as_ref().and_then(|c| c.as_ref().ok().copied()),
            cone_error: cone.and_then(|c| c.err()),
        }
    }
}

/// `get_property_info`, as JSON and as the readable block of
/// `report -detail`.
#[derive(Debug, Clone, Serialize)]
pub struct PropertyInfo {
    pub name: String,
    pub kind: &'static str,
    pub task: String,
    pub enabled: bool,
    pub status: &'static str,
    pub expr: String,
    /// The vacuity checks' word, "not checked" for a proven assertion they
    /// did not look at, empty otherwise.
    pub vacuity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<ProofStats>,
    pub engines: Vec<EngineRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cone: Option<Cone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cone_error: Option<String>,
}

fn ms(v: u64) -> String {
    if v < 1000 {
        format!("{v} ms")
    } else {
        format!("{:.2} s", v as f64 / 1000.0)
    }
}

fn kib(v: u64) -> String {
    if v < 10 * 1024 {
        format!("{v} KiB")
    } else {
        format!("{:.1} MiB", v as f64 / 1024.0)
    }
}

const NOT: &str = "not measured";

impl PropertyInfo {
    /// The readable block (`report -detail`).
    pub fn text(&self) -> String {
        let mut rows: Vec<(&str, String)> = vec![
            ("property", self.name.clone()),
            ("type", self.kind.to_string()),
            ("task", self.task.clone()),
            (
                "status",
                if self.enabled {
                    self.status.to_string()
                } else {
                    format!("{} (disabled)", self.status)
                },
            ),
        ];
        match &self.stats {
            None => rows.push(("statistics", "none (not proved yet)".into())),
            Some(s) => {
                rows.push(("engine", s.engine.clone().unwrap_or_else(|| "-".into())));
                rows.push(("wall time", ms(s.wall_ms)));
                rows.push(("cpu time", s.cpu_ms.map_or(NOT.into(), ms)));
                rows.push((
                    "peak memory",
                    s.peak_rss_kb
                        .map_or("not measured (below an earlier peak)".into(), kib),
                ));
                rows.push((
                    "depth",
                    match (s.depth, s.depth_kind) {
                        (Some(d), Some(k)) => format!("{d} ({k})"),
                        _ => NOT.into(),
                    },
                ));
                rows.push((
                    "deepest bound",
                    s.max_bound.map_or(NOT.into(), |b| b.to_string()),
                ));
                rows.push((
                    "invariant clauses",
                    s.invariant_clauses
                        .map_or("none given".into(), |c| c.to_string()),
                ));
                rows.push((
                    "evidence",
                    s.evidence.clone().unwrap_or_else(|| "none given".into()),
                ));
                rows.push((
                    "certificate",
                    s.certificate
                        .clone()
                        .unwrap_or_else(|| "not asked for".into()),
                ));
            }
        }
        if !self.vacuity.is_empty() {
            rows.push(("vacuity", self.vacuity.to_string()));
        }
        match (&self.cone, &self.cone_error) {
            (Some(c), _) => rows.push((
                "cone of influence",
                format!(
                    "{} state(s), {} bits; {} input(s), {} bits",
                    c.states, c.state_bits, c.inputs, c.input_bits
                ),
            )),
            (None, Some(e)) => rows.push(("cone of influence", format!("not computed: {e}"))),
            _ => {}
        }
        let mut out = String::new();
        for (k, v) in rows {
            out += &format!("  {k:<18} {v}\n");
        }
        if !self.engines.is_empty() {
            out += "  engines:\n";
            for r in &self.engines {
                let outcome = serde_json::to_value(r.outcome)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                out += &format!(
                    "    {:<22} {:<8} {:>10}  {}\n",
                    r.engine,
                    outcome,
                    ms(r.ms as u64),
                    r.depth.map_or(String::new(), |d| format!("depth {d}"))
                );
            }
        }
        out
    }
}

impl DesignInfo {
    /// The readable table (`report -design`).
    pub fn text(&self) -> String {
        let mut rows: Vec<(String, String)> = Vec::new();
        let mut row = |k: &str, v: String| rows.push((k.to_string(), v));
        row("top", self.top.clone().unwrap_or_else(|| "-".into()));
        row("design files", self.files.to_string());
        match &self.design {
            None if !self.elaborated => row("design", "not elaborated".into()),
            None => row("design", "not counted (the design does not lower)".into()),
            Some(d) => {
                row(
                    "instances",
                    format!("{} of {} module(s)", d.instances, d.modules),
                );
                row("inputs", format!("{} ({} bits)", d.inputs, d.input_bits));
                row("outputs", format!("{} ({} bits)", d.outputs, d.output_bits));
                row("flops", format!("{} ({} bits)", d.flops, d.flop_bits));
                row("latches", format!("{} ({} bits)", d.latches, d.latch_bits));
                let mems = d
                    .memories
                    .iter()
                    .map(|m| format!("{} {}x{}", m.name, m.words, m.width))
                    .collect::<Vec<_>>();
                row(
                    "memories",
                    match mems.len() {
                        0 => "0".into(),
                        n => format!("{n}: {}", mems.join(", ")),
                    },
                );
                let clocks = d
                    .clocks
                    .iter()
                    .map(|c| format!("{} ({}, {} reg)", c.signal, c.edges.join("/"), c.registers))
                    .collect::<Vec<_>>();
                row(
                    "clocks found",
                    if clocks.is_empty() {
                        "none".into()
                    } else {
                        clocks.join(", ")
                    },
                );
                let resets = d
                    .resets
                    .iter()
                    .map(|r| {
                        format!(
                            "{} (active {}, {} reg)",
                            r.signal,
                            if r.active_high { "high" } else { "low" },
                            r.registers
                        )
                    })
                    .collect::<Vec<_>>();
                row(
                    "async resets found",
                    if resets.is_empty() {
                        "none".into()
                    } else {
                        resets.join(", ")
                    },
                );
                row("undriven ranges", d.undriven.to_string());
            }
        }
        row(
            "clocks declared",
            if self.declared_clocks.is_empty() {
                "none".into()
            } else {
                self.declared_clocks.join(", ")
            },
        );
        row(
            "reset declared",
            self.declared_reset.clone().unwrap_or_else(|| "none".into()),
        );
        row(
            "black boxes",
            if self.black_boxes.is_empty() {
                "none".into()
            } else {
                self.black_boxes.join(", ")
            },
        );
        row("task", self.task.clone());
        row(
            "cut points",
            if self.cut_points.is_empty() {
                "none".into()
            } else {
                self.cut_points.join(", ")
            },
        );
        let p = &self.properties;
        row(
            "properties",
            format!(
                "{} assert, {} cover, {} assumption(s) in force, {} disabled",
                p.asserts, p.covers, p.assumptions, p.disabled
            ),
        );
        match (&self.model, &self.model_error) {
            (Some(m), _) => {
                row("model nodes", format!("{} (Btor2)", m.nodes));
                row(
                    "model states",
                    format!(
                        "{} ({} bits{})",
                        m.states,
                        m.state_bits,
                        if m.array_states > 0 {
                            format!(", {} arrays", m.array_states)
                        } else {
                            String::new()
                        }
                    ),
                );
                row(
                    "model inputs",
                    format!("{} ({} bits)", m.inputs, m.input_bits),
                );
                row(
                    "model targets",
                    format!(
                        "{} bad, {} justice, {} constraint(s)",
                        m.bad, m.justice, m.constraints
                    ),
                );
                row(
                    "AIG",
                    match (&m.aig, &m.aig_error) {
                        (Some(a), _) => format!(
                            "{} and(s), {} latch(es), {} input(s)",
                            a.ands, a.latches, a.inputs
                        ),
                        (None, Some(e)) => format!("not built: {e}"),
                        _ => "not built".into(),
                    },
                );
            }
            (None, Some(e)) => row("model", format!("not built: {e}")),
            _ => row("model", "not built (elaborate first)".into()),
        }
        let mut out = String::new();
        for (k, v) in rows {
            out += &format!("{k:<20} {v}\n");
        }
        out
    }
}

/// [`Vacuity`] as `report -detail` says it.
pub fn vacuity_word(v: Option<Vacuity>) -> &'static str {
    v.map_or("not checked", Vacuity::word)
}
