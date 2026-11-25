use super::types::*;
use super::{drop_assumptions, has_target, only_target, target_name, trace_model};
use crate::progress::Tracker;
use informal_engine::portfolio::EngineRun;
use informal_engine::{Engine, LemmaHub};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Session {
    fn prepare_target_btor(
        &self,
        i: usize,
        lowered: Result<(&str, &[informal_svlang::RtlProperty]), &str>,
    ) -> Result<PathBuf, String> {
        let in_rtl = self.props[i].rtl || self.props[i].copy_of.is_some();
        let check = self.check_name(i);
        let names = match in_rtl {
            true => Ok(()),
            false => self.check_names(&self.props[i].expr),
        };
        names.and_then(|()| {
            let Ok((_, found)) = lowered else {
                return Ok(());
            };
            match found.iter().find(|f| target_name(&f.name) == check) {
                None if in_rtl => Err("not in the model (inside a black box)".to_string()),
                None => Err("not in the model (disabled)".to_string()),
                Some(f) => match (&f.unsupported, &f.error) {
                    (Some(why), _) => Err(format!("not supported yet: {why}")),
                    (None, Some(e)) => Err(e.clone()),
                    (None, None) => Ok(()),
                },
            }
        })?;
        let out = self.workdir.join(format!("prop_{i}.btor2"));
        match lowered {
            Err(e) => Err(e.to_string()),
            Ok((btor, _)) => {
                let keep = check;
                if !has_target(btor, &keep) {
                    Err(format!("no check named {keep} in the model"))
                } else {
                    let task_of_prop = &self.props[i].task;
                    let self_helpers: Vec<String> = self
                        .props
                        .iter()
                        .filter(|p| {
                            p.enabled
                                && p.kind == Kind::Assume
                                && &p.task == task_of_prop
                                && p.from_assert.as_deref() == Some(keep.as_str())
                        })
                        .map(|p| target_name(&p.name))
                        .collect();
                    let (model, section) = informal_svlang::split_trace(btor);
                    let mut per_model = only_target(model, &keep);
                    if !self_helpers.is_empty() {
                        per_model = drop_assumptions(&per_model, &self_helpers);
                    }
                    let mut r = std::fs::write(&out, per_model);
                    if r.is_ok() && !section.is_empty() {
                        let mut per_trace = only_target(btor, &keep);
                        if !self_helpers.is_empty() {
                            per_trace = drop_assumptions(&per_trace, &self_helpers);
                        }
                        r = std::fs::write(trace_model(&out), per_trace);
                    }
                    r.map(|()| out).map_err(|e| e.to_string())
                }
            }
        }
    }

    fn record_target_result(
        &mut self,
        i: usize,
        slot: usize,
        tracker: Option<&Tracker>,
        outcome: (
            Status,
            Option<String>,
            Vec<EngineRun>,
            Option<crate::stats::ProofStats>,
        ),
    ) -> bool {
        let (mut status, trace, runs, mut stats) = outcome;
        let halt = self.stop.load(Ordering::SeqCst);
        if halt && matches!(status, Status::Undetermined { .. }) {
            status = Status::Undetermined {
                reason: "stopped".into(),
            };
        }
        if let Some(st) = &mut stats {
            if matches!(status, Status::Undetermined { .. } | Status::Error { .. }) {
                st.engine = None;
                if st.depth_kind != Some("bound") {
                    st.depth = st.max_bound;
                    st.depth_kind = st.max_bound.map(|_| "bound");
                }
            }
        }
        if let Some(t) = tracker {
            t.finished(slot, &status, &runs);
        }
        self.props[i].status = status;
        self.props[i].trace = trace;
        self.props[i].engines = runs;
        self.props[i].stats = stats;
        self.publish_snapshot();
        halt
    }

    /// The per-property loop of [`Self::prove`], supporting single-pass and
    /// two-pass (`Effort::Low` sweep -> `self.effort` convergence) scheduling.
    /// Returns `(targets_processed, stopped_by_user)`.
    pub(super) fn prove_targets(
        &mut self,
        targets: &[usize],
        lowered: Result<(&str, &[informal_svlang::RtlProperty]), &str>,
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
        base: usize,
        batch_deadline: Option<Instant>,
    ) -> (usize, bool) {
        let lemma_hub = LemmaHub::is_enabled().then(|| Arc::new(LemmaHub::new()));
        let res = self.prove_targets_with_hub(
            targets,
            lowered,
            engines,
            tracker,
            base,
            batch_deadline,
            lemma_hub.as_ref(),
        );
        self.last_lemma_stats = lemma_hub.as_ref().map(|h| h.stats());
        if let Some(st) = &self.last_lemma_stats {
            if std::env::var_os("INFORMAL_IC3_STATS").is_some()
                && (st.published_standing > 0 || st.accepted > 0)
            {
                eprintln!(
                    "lemma_hub: published {} standing / {} frame, accepted {} / rejected {} (dedup {})",
                    st.published_standing,
                    st.published_frame,
                    st.accepted,
                    st.rejected,
                    st.deduplicated
                );
            }
        }
        res
    }

    #[allow(clippy::too_many_arguments)]
    fn prove_targets_with_hub(
        &mut self,
        targets: &[usize],
        lowered: Result<(&str, &[informal_svlang::RtlProperty]), &str>,
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
        base: usize,
        batch_deadline: Option<Instant>,
        lemma_hub: Option<&Arc<LemmaHub>>,
    ) -> (usize, bool) {
        let budget_for = |cap: Duration| -> Option<Duration> {
            match batch_deadline {
                None => Some(cap),
                Some(dl) => {
                    let rem = dl.saturating_duration_since(Instant::now());
                    (!rem.is_zero()).then(|| cap.min(rem))
                }
            }
        };
        let two_pass = match self.orchestration {
            Orchestration::Off => false,
            Orchestration::On => self.prove_bound.is_none(),
            Orchestration::Auto => {
                targets.len() > 1
                    && self.prove_bound.is_none()
                    && self.time_limit >= Duration::from_millis(400)
            }
        };
        if !two_pass {
            if let Some(t) = tracker {
                t.set_phase(None);
            }
            for (k, &i) in targets.iter().enumerate() {
                let slot = base + k;
                if self.stop.load(Ordering::SeqCst) {
                    return (k, true);
                }
                let Some(prop_timeout) = budget_for(self.time_limit) else {
                    self.mark_total_timeout(&targets[k..], tracker, base + k);
                    return (targets.len(), false);
                };
                if let Some(t) = tracker {
                    t.running(slot);
                }
                self.props[i].trace = None;
                let outcome = match self.prepare_target_btor(i, lowered) {
                    Err(message) => (Status::Error { message }, None, Vec::new(), None),
                    Ok(out) => self.check_one_with(
                        engines,
                        i,
                        &out,
                        tracker.map(|t| (t, slot)),
                        self.effort,
                        prop_timeout,
                        true,
                        lemma_hub.cloned(),
                    ),
                };
                if self.record_target_result(i, slot, tracker, outcome) {
                    return (k + 1, true);
                }
            }
            return (targets.len(), false);
        }

        // Two-pass orchestration:
        // Pass 1 sweeps all targets at Effort::Low with a short per-property cap.
        if let Some(t) = tracker {
            t.set_phase(Some("sweep"));
        }
        let sweep_cap = (self.time_limit / 10)
            .max(Duration::from_millis(250).min(self.time_limit / 2))
            .min(Duration::from_secs(2));
        let mut pending: Vec<(usize, usize, PathBuf, Duration)> = Vec::new();
        for (k, &i) in targets.iter().enumerate() {
            let slot = base + k;
            if self.stop.load(Ordering::SeqCst) {
                for &(pk, pi, _, _) in &pending {
                    self.mark_stopped(&[pi]);
                    let _ = pk;
                }
                return (k, true);
            }
            let cap = if self.props[i].bound.is_some() {
                self.time_limit
            } else {
                sweep_cap
            };
            let Some(p1_timeout) = budget_for(cap) else {
                for &(pk, pi, _, _) in &pending {
                    self.mark_total_timeout(&[pi], tracker, base + pk);
                }
                self.mark_total_timeout(&targets[k..], tracker, base + k);
                return (targets.len(), false);
            };
            if let Some(t) = tracker {
                t.running(slot);
            }
            self.props[i].trace = None;
            let out = match self.prepare_target_btor(i, lowered) {
                Err(message) => {
                    let _ = self.record_target_result(
                        i,
                        slot,
                        tracker,
                        (Status::Error { message }, None, Vec::new(), None),
                    );
                    continue;
                }
                Ok(out) => out,
            };
            if self.props[i].bound.is_some() {
                let outcome = self.check_one_with(
                    engines,
                    i,
                    &out,
                    tracker.map(|t| (t, slot)),
                    self.effort,
                    p1_timeout,
                    true,
                    lemma_hub.cloned(),
                );
                if self.record_target_result(i, slot, tracker, outcome) {
                    for &(_, pi, _, _) in &pending {
                        self.mark_stopped(&[pi]);
                    }
                    return (k + 1, true);
                }
                continue;
            }
            let t0 = Instant::now();
            let (status, trace, runs, stats) = self.check_one_with(
                engines,
                i,
                &out,
                tracker.map(|t| (t, slot)),
                Effort::Low,
                p1_timeout,
                false,
                lemma_hub.cloned(),
            );
            let spent = t0.elapsed();
            if self.stop.load(Ordering::SeqCst) {
                let _ = self.record_target_result(i, slot, tracker, (status, trace, runs, stats));
                for &(_, pi, _, _) in &pending {
                    self.mark_stopped(&[pi]);
                }
                return (k + 1, true);
            }
            if !matches!(status, Status::Undetermined { .. }) {
                let _ = self.record_target_result(i, slot, tracker, (status, trace, runs, stats));
            } else {
                self.props[i].status = status;
                self.props[i].engines = runs;
                self.props[i].stats = stats;
                if let Some(t) = tracker {
                    t.defer(slot);
                }
                pending.push((k, i, out, spent));
            }
        }

        // Pass 2 converges remaining undetermined targets at full `self.effort`.
        if !pending.is_empty() {
            if let Some(t) = tracker {
                t.set_phase(Some("converge"));
            }
        }
        for (p_idx, &(k, i, ref out, spent)) in pending.iter().enumerate() {
            let slot = base + k;
            if self.stop.load(Ordering::SeqCst) {
                for &(_, pi, _, _) in &pending[p_idx..] {
                    self.mark_stopped(&[pi]);
                }
                return (targets.len(), true);
            }
            let rem_prop = self
                .time_limit
                .saturating_sub(spent)
                .max(Duration::from_millis(50).min(self.time_limit));
            let Some(p2_timeout) = budget_for(rem_prop) else {
                for &(pk, pi, _, _) in &pending[p_idx..] {
                    self.mark_total_timeout(&[pi], tracker, base + pk);
                }
                return (targets.len(), false);
            };
            if let Some(t) = tracker {
                t.running(slot);
            }
            let outcome = self.check_one_with(
                engines,
                i,
                out,
                tracker.map(|t| (t, slot)),
                self.effort,
                p2_timeout,
                true,
                lemma_hub.cloned(),
            );
            if self.record_target_result(i, slot, tracker, outcome) {
                for &(_, pi, _, _) in &pending[p_idx + 1..] {
                    self.mark_stopped(&[pi]);
                }
                return (targets.len(), true);
            }
        }
        (targets.len(), false)
    }
}
