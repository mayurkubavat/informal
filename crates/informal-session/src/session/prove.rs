use super::types::*;
use super::vacuity_env::has_constraints;
use crate::progress::Tracker;
use anyhow::{anyhow, bail};
use informal_engine::portfolio::EngineRun;
use informal_engine::{portfolio, CheckRequest, Engine, LemmaHub, ModelFormat, Progress, Verdict};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

impl Session {
    pub(crate) fn active_engines(&self) -> Vec<Arc<dyn Engine>> {
        self.engines()
    }

    fn engines(&self) -> Vec<Arc<dyn Engine>> {
        let picked: Vec<_> = self
            .engines
            .iter()
            .filter(|e| {
                self.engine_filter.is_empty() || self.engine_filter.iter().any(|n| n == e.name())
            })
            .filter(|e| e.capabilities().formats.contains(&ModelFormat::Btor2))
            .cloned()
            .collect();
        if picked.is_empty() {
            // set_engines rejects unknown names, so this is a filter whose
            // engines cannot read Btor2: say so rather than switch quietly.
            if !self.engine_filter.is_empty() {
                self.log(&format!(
                    "none of the selected engines ({}) reads Btor2; using all engines",
                    self.engine_filter.join(", ")
                ));
            }
            return self.engines.clone();
        }
        picked
    }

    /// Names of the engines this session can run.
    pub fn engine_names(&self) -> Vec<String> {
        self.engines.iter().map(|e| e.name().to_string()).collect()
    }

    /// Prove the selected assertions and covers (the enabled ones of the
    /// current task when `which` is empty). Returns their indices.
    ///
    /// Properties are proved task by task, each task on its own model: the
    /// design with that task's assumptions and cut points (decision D20).
    ///
    /// Stop (`self.stop`) ends the run: the running property's portfolio is
    /// cancelled, and it and every property not reached yet become
    /// undetermined with reason "stopped". Elaboration and model building
    /// (svlang) are not interrupted; a Stop during them takes effect
    /// at the first engine run.
    pub fn prove(&mut self, which: &[usize]) -> anyhow::Result<Vec<usize>> {
        if self.dry_run {
            return Ok(Vec::new());
        }
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let targets: Vec<usize> = if which.is_empty() {
            (0..self.props.len())
                .filter(|&i| {
                    let p = &self.props[i];
                    p.enabled && p.kind != Kind::Assume && p.task == self.current_task
                })
                .collect()
        } else {
            which
                .iter()
                .copied()
                .filter(|&i| self.props[i].kind != Kind::Assume)
                .collect()
        };
        if targets.is_empty() {
            if let Some(why) = self
                .elab
                .as_ref()
                .map(|d| &d.not_lowered)
                .filter(|w| !w.is_empty())
            {
                bail!(
                    "nothing was proved: the design cannot be modelled\n{}",
                    why.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
                );
            }
        }
        // Task by task, in the order the tasks were made; the tracker's
        // slots follow that order.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for t in self.tasks.iter().map(|t| t.name.clone()) {
            let members: Vec<usize> = targets
                .iter()
                .copied()
                .filter(|&i| self.props[i].task == t)
                .collect();
            if !members.is_empty() {
                groups.push((t, members));
            }
        }
        let targets: Vec<usize> = groups.iter().flat_map(|g| g.1.clone()).collect();
        let tracker = self.sink.clone().map(|sink| {
            Tracker::start(
                sink,
                targets
                    .iter()
                    .map(|&i| (self.props[i].name.clone(), self.props[i].kind.label())),
            )
        });
        let engines = self.engines();
        self.environment = None;
        self.frontend = "svlang".into();

        // Heartbeat for the UI while the loop runs, so elapsed times move
        // between property events.
        let batch_deadline = self.total_time_limit.map(|d| Instant::now() + d);
        let looping = AtomicBool::new(true);
        let stopped = std::thread::scope(|sc| {
            if let Some(t) = &tracker {
                let looping = &looping;
                sc.spawn(move || {
                    let mut n = 0u32;
                    while looping.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(100));
                        n += 1;
                        if n.is_multiple_of(5) && looping.load(Ordering::SeqCst) {
                            t.tick();
                        }
                    }
                });
            }
            let mut base = 0;
            let mut stopped = false;
            for (task, members) in &groups {
                if stopped || self.stop.load(Ordering::SeqCst) {
                    stopped = true;
                    self.mark_stopped(members);
                    base += members.len();
                    continue;
                }
                if batch_deadline.is_some_and(|dl| Instant::now() >= dl) {
                    self.mark_total_timeout(members, tracker.as_ref(), base);
                    base += members.len();
                    continue;
                }
                stopped = self.prove_task(
                    task,
                    members,
                    &engines,
                    tracker.as_ref(),
                    base,
                    batch_deadline,
                );
                base += members.len();
            }
            looping.store(false, Ordering::SeqCst);
            stopped
        });
        if let Some(t) = &tracker {
            t.end(stopped || self.stop.load(Ordering::SeqCst));
        }
        Ok(targets)
    }

    /// Properties a Stop kept from running.
    pub(super) fn mark_stopped(&mut self, which: &[usize]) {
        for &i in which {
            self.props[i].status = Status::Undetermined {
                reason: "stopped".into(),
            };
            self.props[i].trace = None;
            self.props[i].witness_trace = None;
            self.props[i].engines.clear();
            self.props[i].stats = None;
        }
    }

    /// Properties skipped because `total_time_limit` expired.
    pub(super) fn mark_total_timeout(
        &mut self,
        which: &[usize],
        tracker: Option<&Tracker>,
        base: usize,
    ) {
        for (offset, &i) in which.iter().enumerate() {
            let status = Status::Undetermined {
                reason: "total time limit".into(),
            };
            if let Some(t) = tracker {
                t.finished(base + offset, &status, &self.props[i].engines);
            }
            self.props[i].status = status;
            self.props[i].trace = None;
            self.props[i].witness_trace = None;
        }
    }

    /// Prove `targets`, all of `task`, on that task's model: the
    /// environment check, the per-property loop and the vacuity checks.
    /// `base` is the first target's slot in the tracker. Returns whether a
    /// Stop ended it.
    fn prove_task(
        &mut self,
        task: &str,
        targets: &[usize],
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
        base: usize,
        batch_deadline: Option<Instant>,
    ) -> bool {
        let lowered = self.lower(task);
        let mut environment = None;
        if let Ok((btor, _)) = &lowered {
            let assumes = self
                .props
                .iter()
                .any(|p| p.enabled && p.kind == Kind::Assume && p.task == task);
            if self.environment_depth > 0 && assumes && has_constraints(btor) {
                let env = self.check_environment(task, btor, self.environment_depth);
                self.report_environment(&env);
                environment = Some(env);
            }
        }
        let under_ai = self.ai_in_force_for(task);
        for &i in targets {
            self.props[i].under_ai = under_ai.clone();
        }
        let (reached, stopped_in_loop) = self.prove_targets(
            targets,
            lowered
                .as_ref()
                .map(|(btor, rtl)| (btor.as_str(), rtl.as_slice()))
                .map_err(String::as_str),
            engines,
            tracker,
            base,
            batch_deadline,
        );
        // A Stop that came while the last property finished still counts.
        let stopped = stopped_in_loop || self.stop.load(Ordering::SeqCst);
        if stopped && reached < targets.len() {
            self.mark_stopped(&targets[reached..]);
        }
        // A conflict in the environment empties every proof of this run;
        // otherwise each proven assertion's precondition is covered.
        let conflict = environment.as_ref().is_some_and(Environment::conflict);
        if environment.is_some() {
            // The last check made is the one the session shows.
            self.environment = environment;
        }
        for &i in targets {
            self.props[i].vacuity = match (&self.props[i].status, conflict) {
                (Status::Proven { .. }, true) => Some(Vacuity::VacuousEnvironment),
                _ => None,
            };
            self.props[i].witness_trace = None;
        }
        if let (Ok((btor, _)), false, true) = (&lowered, conflict, self.preconditions) {
            if targets[..reached].iter().any(|&i| {
                matches!(self.props[i].status, Status::Proven { .. })
                    && self.props[i].kind == Kind::Assert
            }) {
                if let Some(t) = tracker {
                    t.emit_phase("vacuity");
                }
            }
            for &i in &targets[..reached] {
                if self.stop.load(Ordering::SeqCst)
                    || batch_deadline.is_some_and(|dl| Instant::now() >= dl)
                {
                    break;
                }
                let proven = matches!(self.props[i].status, Status::Proven { .. });
                if proven && self.props[i].kind == Kind::Assert {
                    self.props[i].vacuity = self.check_precondition(i, btor, engines);
                }
            }
            let vacuous: Vec<&str> = targets
                .iter()
                .filter(|&&i| self.props[i].vacuity == Some(Vacuity::Vacuous))
                .map(|&i| self.props[i].name.as_str())
                .collect();
            if !vacuous.is_empty() {
                self.log(&format!(
                    "VACUOUS: {} proven assertion(s) never check anything, because no trace reaches their antecedent (precondition unreachable): {}",
                    vacuous.len(),
                    vacuous.join(", ")
                ));
            }
        }
        let run_witnesses = self.witnesses || self.effort == Effort::Max;
        if let (Ok((btor, _)), false, true) = (&lowered, conflict, run_witnesses) {
            if targets[..reached].iter().any(|&i| {
                matches!(self.props[i].status, Status::Proven { .. })
                    && !matches!(
                        self.props[i].vacuity,
                        Some(Vacuity::Vacuous | Vacuity::VacuousEnvironment)
                    )
                    && self.props[i].kind == Kind::Assert
            }) {
                if let Some(t) = tracker {
                    t.emit_phase("witness");
                }
            }
            for &i in &targets[..reached] {
                if self.stop.load(Ordering::SeqCst)
                    || batch_deadline.is_some_and(|dl| Instant::now() >= dl)
                {
                    break;
                }
                let proven = matches!(self.props[i].status, Status::Proven { .. });
                let vacuous = matches!(
                    self.props[i].vacuity,
                    Some(Vacuity::Vacuous | Vacuity::VacuousEnvironment)
                );
                if proven && !vacuous && self.props[i].kind == Kind::Assert {
                    self.props[i].witness_trace = self.check_witness_cover(i, btor, engines);
                }
            }
        }
        let run_proof_core = self.with_proof_core || self.effort == Effort::Max;
        if run_proof_core && !conflict {
            if targets[..reached].iter().any(|&i| {
                matches!(
                    self.props[i].status,
                    Status::Proven { .. } | Status::Unreachable { .. }
                )
            }) {
                if let Some(t) = tracker {
                    t.emit_phase("proof_core");
                }
            }
            for &i in &targets[..reached] {
                if self.stop.load(Ordering::SeqCst)
                    || batch_deadline.is_some_and(|dl| Instant::now() >= dl)
                {
                    break;
                }
                if matches!(
                    self.props[i].status,
                    Status::Proven { .. } | Status::Unreachable { .. }
                ) {
                    self.props[i].proof_core = crate::proof_core::compute_proof_core(self, i).ok();
                }
            }
        }
        stopped
    }

    /// Run the portfolio on one property's model. With a tracker, engine
    /// progress (k-bound) is forwarded to it for `slot`. A CEX or cover
    /// also gets its trace written (the path is returned with the status),
    /// and every engine's run is returned for the proof profiler.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_one_with(
        &self,
        engines: &[Arc<dyn Engine>],
        i: usize,
        model: &Path,
        tracker: Option<(&Tracker, usize)>,
        effort: Effort,
        timeout: Duration,
        allow_bounded_promote: bool,
        lemma_hub: Option<Arc<LemmaHub>>,
    ) -> (
        Status,
        Option<String>,
        Vec<EngineRun>,
        Option<crate::stats::ProofStats>,
    ) {
        let kind = self.props[i].kind;
        let text = std::fs::read_to_string(model).unwrap_or_default();
        let has = |op: &str| {
            text.lines()
                .filter(|l| l.split_whitespace().nth(1) == Some(op))
                .count()
        };
        let bads = has("bad") + has("justice");
        let filtered = informal_engine::selector::select_for_btor_with_effort(
            engines,
            &text,
            !self.engine_filter.is_empty(),
            effort,
        );
        let engines = &filtered[..];
        if bads == 0 {
            // The front end folded the check to a constant true.
            let status = match kind {
                Kind::Cover => Status::Unreachable {
                    engine: "elaboration".into(),
                    ms: 0,
                },
                _ => Status::Proven {
                    engine: "elaboration".into(),
                    ms: 0,
                },
            };
            return (status, None, Vec::new(), None);
        }
        if let Some(bound) = self.props[i].bound.or(self.prove_bound) {
            let bmc_eng: Arc<dyn Engine> = Arc::new(informal_bmc::Bmc {
                max_depth: Some(bound as usize),
                step: 1,
            });
            let bounded_engines = [bmc_eng];
            let req = CheckRequest {
                model: model.to_path_buf(),
                format: ModelFormat::Btor2,
                timeout,
                effort,
                max_depth: Some(bound),
            };
            let before = crate::stats::Usage::now();
            let o = portfolio::run_until(&bounded_engines, &req, None, Some(self.stop.as_ref()));
            let usage = before.zip(crate::stats::Usage::now());
            let verdict = o
                .winner
                .as_ref()
                .map(|w| w.verdict.clone())
                .or_else(|| {
                    o.runs
                        .first()
                        .and_then(|r| r.as_ref().ok())
                        .map(|r| r.verdict.clone())
                })
                .unwrap_or_else(|| portfolio::verdict(&o));
            let ms = o.wall_ms;
            let mut status = match (&verdict, kind) {
                (Verdict::Safe, Kind::Cover) => Status::Unreachable {
                    engine: "informal-bmc".into(),
                    ms,
                },
                (Verdict::Safe, _) => Status::Proven {
                    engine: "informal-bmc".into(),
                    ms,
                },
                (Verdict::Unsafe { depth }, Kind::Cover) => Status::Covered {
                    engine: "informal-bmc".into(),
                    depth: *depth,
                    ms,
                },
                (Verdict::Unsafe { depth }, _) => Status::Cex {
                    engine: "informal-bmc".into(),
                    depth: *depth,
                    ms,
                },
                (Verdict::Unknown { reason }, Kind::Assert) if reason.starts_with("bound") => {
                    Status::BoundedProven {
                        engine: "informal-bmc".into(),
                        bound,
                        ms,
                    }
                }
                (Verdict::Unknown { reason }, _) => Status::Undetermined {
                    reason: reason.clone(),
                },
            };
            let mut trace = None;
            if let Status::Cex { depth, .. } | Status::Covered { depth, .. } = &mut status {
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                if let Ok((path, d)) = self.write_trace(&self.props[i].name, model, witness, *depth)
                {
                    depth.get_or_insert(d as u32);
                    trace = Some(path);
                }
            }
            let stats = crate::stats::ProofStats::of_run(&status, &o.profile, ms, None, usage);
            return (status, trace, o.profile, Some(stats));
        }
        let cache_ctx = if self.prove_cache && self.certificates.is_none() {
            let dir = self
                .prove_cache_dir
                .clone()
                .unwrap_or_else(|| self.workdir.join("prove_cache"));
            let key = crate::cache::cache_key(kind, &text);
            if self.prove_cache_mode.can_read() {
                if let Some((cached_status, cached_stats)) = crate::cache::lookup(&dir, &key, kind)
                {
                    return (cached_status, None, Vec::new(), Some(cached_stats));
                }
            }
            Some((dir, key))
        } else {
            None
        };
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout,
            effort,
            max_depth: self.max_trace_length,
        };
        let stop = Some(self.stop.as_ref());
        let before = crate::stats::Usage::now();
        let o = match tracker {
            None => portfolio::run_with_hub(engines, &req, None, stop, lemma_hub),
            Some((t, slot)) => std::thread::scope(|sc| {
                let (tx, rx) = mpsc::channel::<Progress>();
                // Ends when the portfolio returns and drops the last sender.
                sc.spawn(move || {
                    for p in rx {
                        match p.output {
                            Some((text, err)) => t.engine_line(slot, &p.engine, text, err),
                            None => t.bound(slot, &p.engine, p.bound),
                        }
                    }
                });
                portfolio::run_with_hub(engines, &req, Some(tx), stop, lemma_hub)
            }),
        };
        let usage = before.zip(crate::stats::Usage::now());
        let engine = o
            .winner
            .as_ref()
            .map(|w| w.engine.clone())
            .unwrap_or_default();
        let ms = o.wall_ms;
        let mut certificate = None;
        let winner = engine.clone();
        let verdict = portfolio::verdict(&o);
        if let (Some(dir), Some(w), true) = (&self.certificates, &o.winner, verdict.is_definitive())
        {
            // A cover's trace and an unreachable cover's proof are
            // certificates like an assertion's.
            let cert = w.certificate.as_ref();
            match informal_bmc::certificate::write(dir, model, &w.engine, &verdict, cert) {
                Ok(s) => {
                    certificate = Some(s.line());
                    self.log(&format!("{}: {}", self.props[i].name, s.line()))
                }
                Err(e) => {
                    certificate = Some(format!("not written: {e:#}"));
                    self.log(&format!(
                        "{}: certificate not written: {e:#}",
                        self.props[i].name
                    ))
                }
            }
        }
        let status = match (verdict, kind) {
            (Verdict::Safe, Kind::Cover) => Status::Unreachable { engine, ms },
            (Verdict::Safe, _) => Status::Proven { engine, ms },
            (Verdict::Unsafe { depth }, Kind::Cover) => Status::Covered { engine, depth, ms },
            (Verdict::Unsafe { depth }, _) => Status::Cex { engine, depth, ms },
            (Verdict::Unknown { reason }, Kind::Assert)
                if allow_bounded_promote && !self.stop.load(Ordering::SeqCst) =>
            {
                if let Some((bound, bmc_eng)) = self.max_trace_length.and_then(|b| {
                    o.runs
                        .iter()
                        .filter_map(|r| r.as_ref().ok())
                        .find(|r| {
                            matches!(&r.verdict, Verdict::Unknown { reason } if reason.starts_with("bound"))
                        })
                        .map(|r| (b, r.engine.clone()))
                }) {
                    Status::BoundedProven {
                        engine: bmc_eng,
                        bound,
                        ms,
                    }
                } else {
                    Status::Undetermined { reason }
                }
            }
            (Verdict::Unknown { reason }, _) => Status::Undetermined { reason },
        };
        let mut status = status;
        let mut trace = None;
        let mut withdrawn = None;
        if let Status::Cex { depth, .. } | Status::Covered { depth, .. } = &mut status {
            if !self.stop.load(Ordering::SeqCst) {
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                match self.write_trace(&self.props[i].name, model, witness, *depth) {
                    Ok((path, d)) => {
                        // IC3 and external engines often report no depth; the
                        // replayed trace has one.
                        depth.get_or_insert(d as u32);
                        trace = Some(path);
                    }
                    Err(reason) => withdrawn = reason,
                }
            }
        }
        // A counterexample nothing here can reproduce is not reported: the
        // replay is the only check an UNSAFE verdict gets.
        if let Some(why) = withdrawn {
            self.log(&format!(
                "{}: {winner} reported a counterexample that does not replay ({why}); verdict withdrawn",
                self.props[i].name
            ));
            status = Status::Undetermined {
                reason: format!("counterexample did not replay ({winner})"),
            };
        }
        // The profiler's winner row gets the same depth when it had none.
        let mut runs = o.profile;
        if let Status::Cex {
            depth: Some(d),
            engine,
            ..
        }
        | Status::Covered {
            depth: Some(d),
            engine,
            ..
        } = &status
        {
            for r in runs
                .iter_mut()
                .filter(|r| &r.engine == engine && r.depth.is_none())
            {
                r.depth = Some(*d);
            }
        }
        let winner_cert = o.winner.as_ref().and_then(|w| w.certificate.as_ref());
        let mut stats =
            crate::stats::ProofStats::of_run(&status, &runs, o.wall_ms, winner_cert, usage);
        stats.certificate = certificate;
        if let Some((dir, key)) = cache_ctx {
            if self.prove_cache_mode.can_write() {
                crate::cache::store(&dir, &key, kind, &status, Some(&stats));
            }
        }
        (status, trace, runs, Some(stats))
    }
}
