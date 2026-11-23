use super::types::*;
use super::{has_target, only_target, trace_model};
use anyhow::anyhow;
use informal_engine::{portfolio, CheckRequest, Engine, ModelFormat, Verdict};
use std::sync::Arc;
use std::time::Duration;

impl Session {
    /// Cover assertion `i`'s precondition (`<name>:precondition` in the
    /// model): hit by a trace that replays means some attempt was checked;
    /// unreachable means none ever is. `None` when the model has no such
    /// cover (an assertion with no antecedent, an `until`).
    pub(super) fn check_precondition(
        &self,
        i: usize,
        btor: &str,
        engines: &[Arc<dyn Engine>],
    ) -> Option<Vacuity> {
        let name = format!("{}:precondition", self.props[i].name);
        let keep = format!("{}:precondition", self.check_name(i));
        if !has_target(btor, &keep) {
            return None;
        }
        let out = self.workdir.join(format!("prop_{i}.precondition.btor2"));
        let (model, section) = informal_svlang::split_trace(btor);
        let isolated = only_target(model, &keep);
        let mut w = std::fs::write(&out, &isolated);
        if w.is_ok() && !section.is_empty() {
            w = std::fs::write(trace_model(&out), only_target(btor, &keep));
        }
        if w.is_err() {
            return Some(Vacuity::NotChecked);
        }
        let req = CheckRequest {
            model: out.clone(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
            effort: self.effort,
            max_depth: self.max_trace_length,
        };
        let safety = informal_bmc::select_for_btor_with_effort(
            engines,
            &isolated,
            !self.engine_filter.is_empty(),
            self.effort,
        );
        let o = portfolio::run_until(&safety, &req, None, Some(self.stop.as_ref()));
        let verdict = portfolio::verdict(&o);
        if let (Some(dir), Some(w), true) = (&self.certificates, &o.winner, verdict.is_definitive())
        {
            match informal_bmc::certificate::write(
                dir,
                &out,
                &w.engine,
                &verdict,
                w.certificate.as_ref(),
            ) {
                Ok(s) => self.log(&format!("{name}: {}", s.line())),
                Err(e) => self.log(&format!("{name}: certificate not written: {e:#}")),
            }
        }
        let cut = !(self.stopats_of(&self.props[i].task).is_empty()
            && self.design.bbox.is_empty()
            && self.design.bbox_instances.is_empty());
        Some(match verdict {
            Verdict::Safe => Vacuity::Vacuous,
            Verdict::Unsafe { depth } => {
                // The replay is the only check a hit gets, as for a cover.
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                match self.write_trace(&name, &out, witness, depth) {
                    Ok(_) if cut => Vacuity::NonVacuousCutPoints,
                    Ok(_) => Vacuity::NonVacuous,
                    Err(_) => Vacuity::NotChecked,
                }
            }
            Verdict::Unknown { .. } => Vacuity::NotChecked,
        })
    }

    /// Run `<name>:witness` (antecedent + consequent) for a proven assertion
    /// when `elaborate -create_related_covers {witness}` is active, writing a
    /// VCD witness trace on hit.
    pub(super) fn check_witness_cover(
        &self,
        i: usize,
        btor: &str,
        engines: &[Arc<dyn Engine>],
    ) -> Option<String> {
        let name = format!("{}:witness", self.props[i].name);
        let keep = format!("{}:witness", self.check_name(i));
        if !has_target(btor, &keep) {
            return None;
        }
        let out = self.workdir.join(format!("prop_{i}.witness.btor2"));
        let (model, section) = informal_svlang::split_trace(btor);
        let isolated = only_target(model, &keep);
        let mut w = std::fs::write(&out, &isolated);
        if w.is_ok() && !section.is_empty() {
            w = std::fs::write(trace_model(&out), only_target(btor, &keep));
        }
        w.ok()?;
        let req = CheckRequest {
            model: out.clone(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
            effort: self.effort,
            max_depth: self.max_trace_length,
        };
        let safety = informal_bmc::select_for_btor_with_effort(
            engines,
            &isolated,
            !self.engine_filter.is_empty(),
            self.effort,
        );
        let o = portfolio::run_until(&safety, &req, None, Some(self.stop.as_ref()));
        let verdict = portfolio::verdict(&o);
        match verdict {
            Verdict::Unsafe { depth } => {
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                match self.write_trace(&name, &out, witness, depth) {
                    Ok((path, _)) => {
                        self.log(&format!("{name}: covered ({path})"));
                        Some(path)
                    }
                    Err(_) => None,
                }
            }
            Verdict::Safe => {
                self.log(&format!("{name}: unreachable"));
                None
            }
            Verdict::Unknown { .. } => None,
        }
    }

    /// The environment check: can the assumptions and the reset run for
    /// `depth` model steps? A trace of n steps that meets every constraint
    /// has prefixes of every shorter length that do too, so the longest
    /// allowed trace is found by bisection, each probe one bounded BMC run
    /// of informal-bmc on the model with no targets but a step counter.
    /// Unless that is shorter than `depth`, the assumptions do not conflict
    /// within it (they may still over-constrain: see docs/vacuity.md).
    pub fn check_environment(&self, task: &str, btor: &str, depth: u32) -> Environment {
        let assumptions: Vec<String> = self
            .props
            .iter()
            .filter(|p| p.enabled && p.kind == Kind::Assume && p.task == task)
            .map(|p| p.name.clone())
            .collect();
        let mut env = Environment {
            task: task.to_string(),
            asked: depth,
            allowed: None,
            assumptions,
            conflict_set: Vec::new(),
        };
        let (model, _) = informal_svlang::split_trace(btor);
        let probe = |n: u32| -> Option<bool> { self.trace_of_length(model, n) };
        match probe(depth) {
            Some(true) => env.allowed = Some(depth),
            None => {}
            Some(false) => {
                // Longest n in [0, depth) with a trace: lo has one (or is
                // -1 for none at all), hi has none.
                let (mut lo, mut hi) = (-1i64, depth as i64);
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    match probe(mid as u32) {
                        Some(true) => lo = mid,
                        Some(false) => hi = mid,
                        None => return env,
                    }
                }
                // No trace of even one step: report 0 allowed steps.
                env.allowed = Some(lo.max(0) as u32);
                if lo < 0 {
                    env.allowed = Some(0);
                    env.asked = env.asked.max(1);
                }
                env.conflict_set = self.conflict_set(model, (lo + 1) as u32);
            }
        }
        env
    }

    /// The assumptions (and the reset) that conflict within `n` steps: of
    /// the named constraint lines, drop each owner in turn and keep it out
    /// while the rest still allow no trace of `n` steps. What is left is a
    /// smallest conflicting set in the sense that dropping any one member
    /// allows a trace (not always the smallest of all such sets). A probe
    /// that cannot decide keeps its owner in the set.
    fn conflict_set(&self, model: &str, n: u32) -> Vec<String> {
        let owners = self.constraint_owners(model, false);
        narrow(model, &owners, |m| {
            self.trace_of_length(m, n) == Some(false)
        })
    }

    /// The constraint lines of `model` (and its `fair` lines when `fair`)
    /// that belong to an assumption or to the reset, by line index, with
    /// the owner's name: the assumption's property name, or `reset`.
    /// svlang names each constraint and fair line after its assumption's
    /// label (`top.m_x`, or `m_x` for a script assumption); a clock's wave
    /// is the model's own and is never listed.
    fn constraint_owners(&self, model: &str, fair: bool) -> Vec<(usize, String)> {
        let assumes: Vec<&str> = self
            .props
            .iter()
            .filter(|p| p.enabled && p.kind == Kind::Assume)
            .map(|p| p.name.as_str())
            .collect();
        model
            .lines()
            .enumerate()
            .filter_map(|(i, l)| {
                let w: Vec<&str> = l.split_whitespace().collect();
                match w.get(1).copied() {
                    Some("constraint") => {}
                    Some("fair") if fair => {}
                    _ => return None,
                }
                let sym = *w.get(3)?;
                if sym.ends_with("_reset") && sym.starts_with("svlang") {
                    return Some((i, "reset".to_string()));
                }
                assumes
                    .iter()
                    .find(|&&a| {
                        a == sym
                            || a.ends_with(&format!(".{sym}"))
                            || sym.ends_with(&format!(".{a}"))
                    })
                    .map(|a| (i, a.to_string()))
            })
            .collect()
    }

    /// Whether `model` (Btor2, its targets ignored) has a trace of `n`
    /// steps (states 0 to n) that meets every constraint. `None` when the
    /// in-house BMC is not available or did not finish.
    fn trace_of_length(&self, model: &str, n: u32) -> Option<bool> {
        let text = environment_model(model, n);
        let path = self.workdir.join(format!("environment_{n}.btor2"));
        std::fs::create_dir_all(&self.workdir).ok()?;
        std::fs::write(&path, text).ok()?;
        let bmc = informal_bmc::Bmc {
            max_depth: Some(n as usize),
            step: 1,
        };
        let req = CheckRequest::new(
            path.clone(),
            ModelFormat::Btor2,
            self.time_limit.min(Duration::from_secs(60)),
        );
        let r = bmc.check(&req, &informal_engine::RunContext::new(req.timeout));
        let _ = std::fs::remove_file(&path);
        match r.ok()?.verdict {
            Verdict::Unsafe { .. } => Some(true),
            // The counter reaches n only at step n, so "not within the bound"
            // and "never" both mean no trace of n steps.
            Verdict::Safe => Some(false),
            Verdict::Unknown { reason } if reason.starts_with("bound") => Some(false),
            Verdict::Unknown { .. } => None,
        }
    }

    /// Model steps held in reset at the start of each trace (`1` for single-clock,
    /// `tick_reset().steps` for multi-edge/multi-clock models).
    pub fn reset_step_count(&self) -> u32 {
        if self.reset.is_none() {
            return 0;
        }
        let tr = self.tick_reset();
        tr.steps.max(1)
    }

    /// Run 2QBF dead-end / trap-state detection on the current task's environment
    /// up to `depth` steps (`check_assumptions -dead_end`).
    pub fn dead_end_now(
        &mut self,
        depth: u32,
    ) -> anyhow::Result<Option<informal_bmc::deadend::DeadEnd>> {
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let task = self.current_task.clone();
        let btor = self.task_model(&task).map_err(|e| anyhow!(e))?;
        let timeout = self.time_limit.min(Duration::from_secs(60));
        informal_bmc::deadend::find_dead_end(&btor, depth, timeout).map_err(|e| anyhow!(e))
    }

    /// Elaborate and lower if needed, then run the environment check to
    /// `depth` steps and keep (and report) its result.
    pub fn environment_now(&mut self, depth: u32) -> anyhow::Result<Environment> {
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let task = self.current_task.clone();
        let (btor, _) = self.lower(&task).map_err(|e| anyhow!(e))?;
        let env = self.check_environment(&task, &btor, depth);
        self.report_environment(&env);
        self.environment = Some(env.clone());
        Ok(env)
    }

    /// Say what the environment check found; a conflict is said loudly.
    pub(super) fn report_environment(&self, env: &Environment) {
        let named = if env.assumptions.is_empty() {
            "the reset and the assumptions of the design".to_string()
        } else {
            format!("the reset and {}", env.assumptions.join(", "))
        };
        let named = match env.task.as_str() {
            EMBEDDED => named,
            t => format!("{named} (task {t})"),
        };
        match env.allowed {
            None => self.log(&format!(
                "environment check: could not tell whether {named} allow {} steps (no in-house BMC, or out of time); proofs are not checked for vacuity",
                env.asked
            )),
            Some(a) if a < env.asked => self.log(&format!(
                "VACUOUS ENVIRONMENT: {named} allow no trace longer than {a} step(s) (asked for {}). Every assertion holds from step {} on whatever the design does, so its proof says nothing beyond that step; {}",
                env.asked,
                a + 1,
                match env.conflict_set.as_slice() {
                    [] => "the constraints carry no names, so the conflict is not narrowed".to_string(),
                    set => format!(
                        "these conflict, and dropping any one of them allows a longer trace: {}",
                        set.join(", ")
                    ),
                }
            )),
            Some(_) => self.note(&format!(
                "environment check: {named} allow traces of {} steps",
                env.asked
            )),
        }
    }

    /// The fairness check (`check_assumptions -live`): is there an infinite
    /// trace of `task`'s model that meets every constraint at every step
    /// and every fairness assumption infinitely often? The model keeps its
    /// constraint and fair lines and gets one liveness target that always
    /// holds, so the liveness engines find a lasso iff such a trace exists
    /// and prove the target iff none does. When none does, a second run
    /// without the fair lines tells whether the fairness assumptions are
    /// the cause or the constraints end every trace on their own. With
    /// `narrow_set`, a vacuous result also gets a conflicting set, found
    /// as the environment check finds its own (one liveness run a probe).
    pub fn check_fairness(&self, task: &str, btor: &str, narrow_set: bool) -> FairEnvironment {
        let (model, _) = informal_svlang::split_trace(btor);
        let owners = self.constraint_owners(model, true);
        let mut fairness: Vec<String> = Vec::new();
        for (i, l) in model.lines().enumerate() {
            let w: Vec<&str> = l.split_whitespace().collect();
            if w.get(1) != Some(&"fair") {
                continue;
            }
            let name = match owners.iter().find(|(j, _)| *j == i) {
                Some((_, o)) => o.clone(),
                None => w.get(3).unwrap_or(&"(unnamed)").to_string(),
            };
            if !fairness.contains(&name) {
                fairness.push(name);
            }
        }
        let mut env = FairEnvironment {
            task: task.to_string(),
            fairness,
            verdict: Fairness::NoFairness,
            conflict_set: Vec::new(),
        };
        if env.fairness.is_empty() {
            return env;
        }
        env.verdict = match self.fair_trace(model) {
            Ok(true) => Fairness::Satisfiable,
            Err(reason) => Fairness::Unknown(reason),
            Ok(false) => {
                let unfair: String = model
                    .lines()
                    .filter(|l| l.split_whitespace().nth(1) != Some("fair"))
                    .flat_map(|l| [l, "\n"])
                    .collect();
                // Undecided here: the contradiction is proven, its cause
                // is not narrowed.
                match self.fair_trace(&unfair) {
                    Ok(false) => Fairness::VacuousConstraints,
                    _ => Fairness::Vacuous,
                }
            }
        };
        if narrow_set && env.verdict.vacuous() {
            env.conflict_set = narrow(model, &owners, |m| self.fair_trace(m) == Ok(false));
        }
        env
    }

    /// Whether `model` (its targets ignored) has an infinite trace that
    /// meets every constraint and every fair line infinitely often:
    /// `Ok(true)` when an engine found one (a lasso), `Ok(false)` when one
    /// proved there is none, and `Err` with the engines' reason otherwise
    /// (a bounded search, out of time, stopped).
    fn fair_trace(&self, model: &str) -> Result<bool, String> {
        let text = fair_model(model);
        let path = self.workdir.join("environment_fair.btor2");
        std::fs::create_dir_all(&self.workdir).map_err(|e| e.to_string())?;
        std::fs::write(&path, &text).map_err(|e| e.to_string())?;
        let req = CheckRequest {
            model: path.clone(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
            effort: self.effort,
            max_depth: self.max_trace_length,
        };
        // A justice target: the liveness engines (informal-klive and those
        // that read justice lines).
        let live = informal_bmc::select_for_btor_with_effort(
            &self.active_engines(),
            &text,
            !self.engine_filter.is_empty(),
            self.effort,
        );
        let o = portfolio::run_until(&live, &req, None, Some(self.stop.as_ref()));
        let _ = std::fs::remove_file(&path);
        match portfolio::verdict(&o) {
            Verdict::Unsafe { .. } => Ok(true),
            Verdict::Safe => Ok(false),
            Verdict::Unknown { reason } => Err(reason),
        }
    }

    /// Elaborate and lower if needed, then run the fairness check on the
    /// current task and report its result.
    pub fn fairness_now(&mut self, narrow_set: bool) -> anyhow::Result<FairEnvironment> {
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let task = self.current_task.clone();
        let (btor, _) = self.lower(&task).map_err(|e| anyhow!(e))?;
        let env = self.check_fairness(&task, &btor, narrow_set);
        self.report_fairness(&env);
        Ok(env)
    }

    /// Say what the fairness check found; vacuity is said loudly.
    pub(super) fn report_fairness(&self, env: &FairEnvironment) {
        let task = match env.task.as_str() {
            EMBEDDED => String::new(),
            t => format!(" of task {t}"),
        };
        let fair = env.fairness.join(", ");
        let set = match env.conflict_set.as_slice() {
            [] => String::new(),
            set => format!(
                "; these conflict, and dropping any one of them allows a fair trace: {}",
                set.join(", ")
            ),
        };
        match &env.verdict {
            Fairness::NoFairness => self.log(&format!(
                "fairness check: the assumptions{task} have no fairness assumption (s_eventually), so there is nothing to check"
            )),
            Fairness::Satisfiable => self.note(&format!(
                "fairness check: an infinite trace meets the constraints{task} and {fair} infinitely often"
            )),
            Fairness::Unknown(reason) => self.log(&format!(
                "fairness check: could not tell whether an infinite trace meets the constraints{task} and {fair} ({reason}), so this shows nothing about the vacuity of liveness proofs"
            )),
            Fairness::Vacuous => self.log(&format!(
                "VACUOUS (fairness): no infinite trace meets the constraints{task} and {fair} infinitely often: the fairness assumptions are unsatisfiable together with the constraints, so every liveness proof under them is vacuous{set}"
            )),
            Fairness::VacuousConstraints => self.log(&format!(
                "VACUOUS ENVIRONMENT (liveness): the constraints{task} allow no infinite trace even without {fair}, so every liveness proof is vacuous; check_assumptions -depth N or -dead_end shows where traces end{set}"
            )),
        }
    }
}

/// Of `owners` (line index, owner name), drop each owner in turn and keep
/// it out while `conflicts` still holds for `model` without the lines of
/// every owner dropped so far. What is left is a smallest conflicting set
/// in the sense that dropping any one member makes `conflicts` fail (not
/// always the smallest of all such sets). Past 64 owners, all are listed.
fn narrow(
    model: &str,
    owners: &[(usize, String)],
    conflicts: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut names: Vec<&str> = Vec::new();
    for (_, o) in owners {
        if !names.contains(&o.as_str()) {
            names.push(o);
        }
    }
    // One probe each; past this many, list them all rather than wait.
    if names.is_empty() || names.len() > 64 {
        return names.into_iter().map(String::from).collect();
    }
    let mut dropped: Vec<&str> = Vec::new();
    let mut kept = Vec::new();
    for &name in &names {
        dropped.push(name);
        let without: String = model
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                !owners
                    .iter()
                    .any(|(j, o)| j == i && dropped.contains(&o.as_str()))
            })
            .flat_map(|(_, l)| [l, "\n"])
            .collect();
        if !conflicts(&without) {
            dropped.pop();
            kept.push(name.to_string());
        }
    }
    kept
}

/// `model` with its targets (bad, justice) dropped and one liveness target
/// that always holds: an engine finds it violated iff some infinite trace
/// meets every constraint and every fair line infinitely often.
fn fair_model(model: &str) -> String {
    let mut out = String::new();
    let mut top = 0u64;
    for l in model.lines() {
        let mut w = l.split_whitespace();
        if let Some(id) = w.next().and_then(|x| x.parse::<u64>().ok()) {
            top = top.max(id);
        }
        if matches!(w.next(), Some("bad" | "justice")) {
            continue;
        }
        out += l;
        out.push('\n');
    }
    out += &format!(
        "{} sort bitvec 1\n{} one {}\n{} justice 1 {} informal_fair_trace\n",
        top + 1,
        top + 2,
        top + 1,
        top + 3,
        top + 2,
    );
    out
}

/// The model has `constraint` lines (assumptions, and the reset).
pub(super) fn has_constraints(btor: &str) -> bool {
    let (model, _) = informal_svlang::split_trace(btor);
    model
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some("constraint"))
}

/// `model` with its targets (bad, justice, fair) dropped, plus a step
/// counter and one `bad` that holds exactly at step `n`: BMC finds it iff
/// some trace of n steps meets every constraint.
fn environment_model(model: &str, n: u32) -> String {
    let mut out = String::new();
    let mut top = 0u64;
    for l in model.lines() {
        let mut w = l.split_whitespace();
        if let Some(id) = w.next().and_then(|x| x.parse::<u64>().ok()) {
            top = top.max(id);
        }
        if matches!(w.next(), Some("bad" | "justice" | "fair")) {
            continue;
        }
        out += l;
        out.push('\n');
    }
    let width = (32 - n.leading_zeros()).max(1) + 1;
    let id = |k: u64| top + k;
    out += &format!(
        "{} sort bitvec {width}\n{} sort bitvec 1\n{} zero {}\n{} state {} informal_environment_step\n{} init {} {} {}\n{} one {}\n{} add {} {} {}\n{} next {} {} {}\n{} constd {} {n}\n{} eq {} {} {}\n{} bad {} informal_environment\n",
        id(1),
        id(2),
        id(3), id(1),
        id(4), id(1),
        id(5), id(1), id(4), id(3),
        id(6), id(1),
        id(7), id(1), id(4), id(6),
        id(8), id(1), id(4), id(7),
        id(9), id(1),
        id(10), id(2), id(4), id(9),
        id(11), id(10),
    );
    out
}
