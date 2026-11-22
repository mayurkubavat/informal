use super::{
    counts, err, find, opt_bool, opt_f64, opt_str, opt_u64, prop_view, req_str, strings, Api,
    ApiError, Mode, Proposal, Reply,
};
use crate::diagnose;
use informal_fpv::{Kind, Property, Session, Shell, Status};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

impl Api {
    // -------------------------------------------------------------- prove

    pub(super) fn prove(&self, args: &Value) -> Reply {
        let names = strings(args, "properties")?;
        let task = opt_str(args, "task")?;
        let limit = match opt_f64(args, "time_limit_s")? {
            Some(t) if t > 0.0 => Some(Duration::from_secs_f64(t)),
            Some(_) => return Err(err("arguments", "time_limit_s must be above 0")),
            None => None,
        };
        let max = self.opts.max_time;
        self.state.inner.lock().unwrap().progress = None;
        self.on_shell(true, move |sh| {
            let mut s = sh.session.borrow_mut();
            let task = match &task {
                Some(t) => Some(s.task_name(t).map_err(|e| err("not_found", e))?),
                None => None,
            };
            let mut which = Vec::new();
            for n in &names {
                let mut m = match &task {
                    Some(t) => {
                        let mut m = s.matching(&informal_fpv::session::qualify(t, n));
                        m.retain(|&i| &s.props[i].task == t);
                        m
                    }
                    None => Vec::new(),
                };
                if m.is_empty() {
                    m = s.matching(n);
                }
                if m.is_empty() {
                    return Err(err("not_found", format!("no property matches {n}")));
                }
                which.extend(m);
            }
            if let (Some(t), true) = (&task, names.is_empty()) {
                which = s
                    .props_of(t)
                    .into_iter()
                    .filter(|&i| s.props[i].enabled && s.props[i].kind != Kind::Assume)
                    .collect();
                if which.is_empty() {
                    return Err(err("rejected", format!("task {t} has nothing to prove")));
                }
            }
            which.sort();
            which.dedup();
            which.retain(|&i| s.props[i].kind != Kind::Assume);
            if !names.is_empty() && which.is_empty() {
                return Err(err(
                    "rejected",
                    "only assumptions were named: nothing to prove",
                ));
            }
            let old = s.time_limit;
            s.time_limit = limit.unwrap_or(old).min(max);
            s.stop.store(false, Ordering::SeqCst);
            let done = s.prove(&which);
            let used = s.time_limit;
            s.time_limit = old;
            let done = done.map_err(|e| err("rejected", format!("{e:#}")))?;
            let results: Vec<&Property> = done.iter().map(|&i| &s.props[i]).collect();
            Ok(json!({
                "schema": "informal.prove/1",
                "time_limit_s": used.as_secs_f64(),
                "stopped": s.stop.load(Ordering::SeqCst),
                "counts": counts(results.iter().copied()),
                "results": results.iter().map(|p| prove_view(p)).collect::<Vec<_>>(),
            }))
        })?
    }

    /// `prove` with `wait: false`: start it and return.
    pub(super) fn prove_start(self: &Arc<Self>, args: &Value) -> Reply {
        let long = self.begin_long()?;
        self.state.inner.lock().unwrap().last_prove = None;
        let (api, args) = (self.clone(), args.clone());
        std::thread::spawn(move || {
            let r = api.prove(&args);
            api.state.inner.lock().unwrap().last_prove = Some(r);
            drop(long);
            api.changed("prove");
        });
        Ok(json!({"schema": "informal.prove/1", "started": true}))
    }

    pub(super) fn prove_status(&self) -> Reply {
        let i = self.state.inner.lock().unwrap();
        let running = self.state.is_busy();
        let mut reply = json!({"schema": "informal.progress/1", "running": running});
        if let Some(p) = &i.progress {
            reply["total"] = json!(p.total);
            reply["done"] = json!(p.done);
            reply["elapsed_ms"] = json!(p.elapsed_ms as u64);
            reply["properties"] = p
                .props
                .iter()
                .map(|q| {
                    let mut v = json!({"name": q.name, "kind": q.kind, "status": q.status});
                    if let Some(e) = &q.engine {
                        v["engine"] = json!(e);
                    }
                    if let Some(d) = q.depth {
                        v["depth"] = json!(d);
                    }
                    if let Some(ms) = q.ms {
                        v["time_ms"] = json!(ms as u64);
                    }
                    v
                })
                .collect();
        }
        match &i.last_prove {
            Some(Ok(r)) if !running => reply["result"] = r.clone(),
            Some(Err(e)) if !running => {
                reply["error"] = json!({"code": e.code, "message": e.message})
            }
            _ => {}
        }
        Ok(reply)
    }

    // ---------------------------------------------------------- proposals

    /// Zero-side-effect scratch-task soundness oracle: tests a candidate
    /// `assume`, `assert` (helper lemma), `cover`, or `cut_point` in an
    /// isolated temporary task before calling `proposal_add`.
    pub(super) fn proposal_validate(&self, args: &Value) -> Reply {
        static VALIDATE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let kind = req_str(args, "kind")?;
        let name = opt_str(args, "name")?;
        let text = match kind.as_str() {
            "assume" | "assert" | "cover" => req_str(args, "expr")?,
            "cut_point" => req_str(args, "signal")?,
            _ => {
                return Err(err(
                    "arguments",
                    "kind must be assume, assert, cover or cut_point",
                )
                .with_hint("Pass `expr` for assume/assert/cover, or `signal` for cut_point."))
            }
        };
        let explicit_targets = args.get("targets").is_some() && !args["targets"].is_null();
        let req_targets = strings(args, "targets")?;
        let check_covers = opt_bool(args, "check_covers")?
            .unwrap_or(matches!(kind.as_str(), "assume" | "cut_point"));
        let check_dead_end = opt_bool(args, "check_dead_end")?.unwrap_or(kind == "assume");
        let depth = opt_u64(args, "depth")?.unwrap_or(20).clamp(1, 100) as u32;
        let max_s = self.opts.max_time.as_secs_f64();
        let limit_s = opt_f64(args, "time_limit_s")?
            .unwrap_or(15.0)
            .clamp(1.0, max_s);
        let limit = Duration::from_secs_f64(limit_s);
        let seq = VALIDATE_SEQ.fetch_add(1, Ordering::Relaxed);
        let scratch = format!("__validate_{seq}");

        self.on_shell(true, move |sh| {
            let mut s = sh.session.borrow_mut();
            if !s.elaborated {
                return Err(err("rejected", "no design is elaborated")
                    .with_tool("session_open")
                    .with_hint("Open and elaborate a design with session_open first."));
            }
            let probe = Proposal {
                id: 0,
                kind: kind.clone(),
                name: name.clone(),
                text: text.clone(),
                rationale: String::new(),
                state: "pending".into(),
                error: None,
            };
            check(&s, &probe)?;

            let overconstraint_warnings = if kind == "assume" {
                diagnose::assume_overconstraint_warnings(&text, &s.design.hierarchy)
            } else {
                Vec::new()
            };

            let prev_task = s.current_task.clone();
            let prev_limit = s.time_limit;
            let prev_env = s.environment.clone();
            let prev_sink = s.sink.take();
            let prev_assumed_len = s.assumed_from_assert.len();
            s.stop.store(false, Ordering::SeqCst);

            let outcome = (|| -> Reply {
                // Resolve targets & covers in the active task before creating scratch.
                let mut target_indices = Vec::new();
                if explicit_targets {
                    for tname in &req_targets {
                        let idx = find(&s, tname)?;
                        if s.props[idx].kind == Kind::Assume {
                            return Err(err(
                                "rejected",
                                format!("target `{tname}` is an assumption, not an assert/cover"),
                            )
                            .with_tool("property_list"));
                        }
                        target_indices.push(idx);
                    }
                } else {
                    target_indices = s
                        .props
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| {
                            p.enabled
                                && p.task == prev_task
                                && p.kind == Kind::Assert
                                && matches!(
                                    p.status,
                                    Status::Cex { .. } | Status::Undetermined { .. }
                                )
                        })
                        .map(|(i, _)| i)
                        .take(6)
                        .collect();
                }

                let cover_indices: Vec<usize> = if check_covers {
                    s.props
                        .iter()
                        .enumerate()
                        .filter(|(i, p)| {
                            p.enabled
                                && p.task == prev_task
                                && p.kind == Kind::Cover
                                && !target_indices.contains(i)
                        })
                        .map(|(i, _)| i)
                        .take(8)
                        .collect()
                } else {
                    Vec::new()
                };

                let assume_indices: Vec<usize> = s
                    .props
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.enabled && p.task == prev_task && p.kind == Kind::Assume)
                    .map(|(i, _)| i)
                    .collect();

                let target_meta: Vec<(String, String)> = target_indices
                    .iter()
                    .map(|&i| (s.props[i].name.clone(), s.props[i].status.word().to_string()))
                    .collect();
                let cover_meta: Vec<(String, String)> = cover_indices
                    .iter()
                    .map(|&i| (s.props[i].name.clone(), s.props[i].status.word().to_string()))
                    .collect();

                s.create_task(&scratch).map_err(|e| err("internal", e))?;
                s.copy_stopats(&prev_task, &scratch)
                    .map_err(|e| err("internal", e))?;
                if !assume_indices.is_empty() {
                    s.copy_into(&scratch, &assume_indices)
                        .map_err(|e| err("internal", e))?;
                }
                let scratch_targets = if !target_indices.is_empty() {
                    s.copy_into(&scratch, &target_indices)
                        .map_err(|e| err("internal", e))?
                } else {
                    Vec::new()
                };
                let scratch_covers = if !cover_indices.is_empty() {
                    s.copy_into(&scratch, &cover_indices)
                        .map_err(|e| err("internal", e))?
                } else {
                    Vec::new()
                };

                s.set_task(&scratch).map_err(|e| err("internal", e))?;
                s.time_limit = limit;

                // Apply candidate in scratch task only.
                let cand_name: Option<String> = match kind.as_str() {
                    "cut_point" => {
                        s.add_ai_cut_point(&text).map_err(|e| err("rejected", e))?;
                        None
                    }
                    "assume" => Some(
                        s.add_property_in(&scratch, Kind::Assume, name.clone(), &text)
                            .map_err(|e| err("rejected", e))?,
                    ),
                    "cover" => Some(
                        s.add_property_in(&scratch, Kind::Cover, name.clone(), &text)
                            .map_err(|e| err("rejected", e))?,
                    ),
                    _ => Some(
                        s.add_property_in(&scratch, Kind::Assert, name.clone(), &text)
                            .map_err(|e| err("rejected", e))?,
                    ),
                };

                // 1. Environment consistency + 2QBF dead-end check for assumptions.
                let mut env_conflict = false;
                let mut env_allowed: Option<u32> = None;
                let mut conflict_set: Vec<String> = Vec::new();
                let mut dead_end_val = json!({"checked": false, "found": false});
                if kind == "assume" {
                    if let Ok(env) = s.environment_now(depth) {
                        env_conflict = env.conflict();
                        env_allowed = env.allowed;
                        conflict_set = env.conflict_set;
                    }
                    if !env_conflict && check_dead_end {
                        match s.dead_end_now(depth.min(10)) {
                            Ok(Some(de)) => {
                                dead_end_val = json!({
                                    "checked": true,
                                    "found": true,
                                    "depth": de.depth,
                                    "trap_state": de.trap_state.iter().take(16).map(|(sig, val)| json!({"signal": sig, "value": val})).collect::<Vec<_>>(),
                                });
                            }
                            Ok(None) => {
                                dead_end_val = json!({"checked": true, "found": false});
                            }
                            Err(e) => {
                                dead_end_val = json!({"checked": false, "found": false, "error": e.to_string()});
                            }
                        }
                    }
                }
                let dead_end_found = dead_end_val["found"].as_bool().unwrap_or(false);

                // 2. If candidate is an assert or cover, prove it directly first.
                let mut candidate_result = Value::Null;
                let mut cand_ok = true;
                let mut cand_vacuous = false;
                let mut cand_promoted = false;
                if let Some(ref cname) = cand_name {
                    if matches!(kind.as_str(), "assert" | "cover") {
                        if let Some(cidx) = s.props.iter().position(|p| p.name == *cname) {
                            let _ = s.prove(&[cidx]);
                            let cp = &s.props[cidx];
                            cand_vacuous = cp.vacuity.is_some_and(|v| v.is_vacuous());
                            cand_ok = match kind.as_str() {
                                "assert" => {
                                    matches!(cp.status, Status::Proven { .. }) && !cand_vacuous
                                }
                                "cover" => matches!(cp.status, Status::Covered { .. }),
                                _ => true,
                            };
                            candidate_result = prop_view(cp, false);
                            // For a proven non-vacuous helper assertion, test promoting it to
                            // see if it closes the requested/failing target assertions!
                            if kind == "assert"
                                && cand_ok
                                && !scratch_targets.is_empty()
                                && s.assume_from_assert(&scratch, &[cidx], None).is_ok()
                            {
                                cand_promoted = true;
                            }
                        }
                    }
                }

                // 3. Prove targets & cover regression in the scratch task.
                let should_check_impact = !env_conflict
                    && !dead_end_found
                    && (matches!(kind.as_str(), "assume" | "cut_point")
                        || (kind == "assert" && cand_promoted));

                let mut newly_proven = Vec::new();
                let mut vacuous_assertions = Vec::new();
                let mut still_failing = Vec::new();
                let mut target_results = Vec::new();
                let mut lost_covers = Vec::new();
                let mut cover_results = Vec::new();

                if should_check_impact {
                    let mut prove_idxs = Vec::new();
                    for sn in &scratch_targets {
                        if let Some(i) = s.props.iter().position(|p| p.name == *sn) {
                            prove_idxs.push(i);
                        }
                    }
                    if matches!(kind.as_str(), "assume" | "cut_point") {
                        for sn in &scratch_covers {
                            if let Some(i) = s.props.iter().position(|p| p.name == *sn) {
                                prove_idxs.push(i);
                            }
                        }
                    }
                    if !prove_idxs.is_empty() {
                        let _ = s.prove(&prove_idxs);
                    }
                    for ((orig_name, orig_status), sn) in
                        target_meta.iter().zip(scratch_targets.iter())
                    {
                        if let Some(p) = s.props.iter().find(|p| p.name == *sn) {
                            let new_status = p.status.word().to_string();
                            let vac = p.vacuity.is_some_and(|v| v.is_vacuous());
                            if matches!(p.status, Status::Proven { .. }) {
                                if vac {
                                    vacuous_assertions.push(orig_name.clone());
                                } else if orig_status != "proven" {
                                    newly_proven.push(orig_name.clone());
                                }
                            } else if matches!(
                                p.status,
                                Status::Cex { .. } | Status::Undetermined { .. } | Status::Error { .. }
                            ) {
                                still_failing.push(orig_name.clone());
                            }
                            target_results.push(json!({
                                "name": orig_name,
                                "before": orig_status,
                                "after": new_status,
                                "vacuity": p.vacuity.map(|v| v.word()).unwrap_or("not checked"),
                            }));
                        }
                    }
                    if matches!(kind.as_str(), "assume" | "cut_point") {
                        for ((orig_name, orig_status), sn) in
                            cover_meta.iter().zip(scratch_covers.iter())
                        {
                            if let Some(p) = s.props.iter().find(|p| p.name == *sn) {
                                let new_status = p.status.word().to_string();
                                if matches!(p.status, Status::Unreachable { .. })
                                    && orig_status != "unreachable"
                                {
                                    lost_covers.push(orig_name.clone());
                                }
                                cover_results.push(json!({
                                    "name": orig_name,
                                    "before": orig_status,
                                    "after": new_status,
                                }));
                            }
                        }
                    }
                }

                // 4. Score verdict + Poka-Yoke next_step guidance.
                let cand_hard_fail = match kind.as_str() {
                    "assert" => {
                        !cand_ok
                            && matches!(
                                candidate_result["status"].as_str(),
                                Some("cex" | "error")
                            )
                            || cand_vacuous
                    }
                    "cover" => matches!(
                        candidate_result["status"].as_str(),
                        Some("unreachable" | "error")
                    ),
                    _ => false,
                };
                let verdict = if env_conflict
                    || dead_end_found
                    || !vacuous_assertions.is_empty()
                    || !lost_covers.is_empty()
                    || cand_hard_fail
                {
                    "reject"
                } else if !overconstraint_warnings.is_empty()
                    || (kind == "assert" && !cand_ok)
                    || (!target_meta.is_empty() && newly_proven.is_empty() && kind != "cover")
                {
                    "warn"
                } else {
                    "pass"
                };

                let ready_to_propose = verdict != "reject";
                let next_step = if env_conflict {
                    format!(
                        "REJECTED: assumption conflicts with the environment (allowed {:?}/{depth} steps; conflict_set: {:?}). Weaken or discard this assumption — do NOT call proposal_add.",
                        env_allowed, conflict_set
                    )
                } else if dead_end_found {
                    "REJECTED: 2QBF dead-end check found a trap state where no legal next input satisfies the assumption. Rewrite as a state-free or protocol-legal primary-input constraint.".into()
                } else if !lost_covers.is_empty() {
                    format!(
                        "REJECTED: assumption over-constrains the design and makes cover(s) [{}] unreachable. Weaken the assumption before proposing.",
                        lost_covers.join(", ")
                    )
                } else if !vacuous_assertions.is_empty() || cand_vacuous {
                    format!(
                        "REJECTED: vacuous proof detected (vacuous assertions: {:?}). Check antecedent reachability and environment constraints.",
                        vacuous_assertions
                    )
                } else if cand_hard_fail {
                    format!(
                        "REJECTED: candidate `{kind}` ended as `{}` (not valid on the RTL). Refine the expression using `cex_diagnose` or `design_source`.",
                        candidate_result["status"].as_str().unwrap_or("failed")
                    )
                } else if verdict == "warn" {
                    if !overconstraint_warnings.is_empty() {
                        "WARNING: candidate passed solver checks, but static hygiene flagged DUT output/internal signal usage in `assume`. Prefer constraining primary inputs only, or justify clearly in `proposal_add` rationale.".into()
                    } else {
                        "WARNING: candidate did not fail, but did not yet close the target assertions. Refine or combine with another helper lemma / abstraction before calling proposal_add.".into()
                    }
                } else {
                    format!(
                        "VALID: passed environment, dead-end, vacuity, and cover regression checks (newly_proven: {:?}). Safe to call proposal_add.",
                        newly_proven
                    )
                };

                Ok(json!({
                    "schema": "informal.validate/1",
                    "verdict": verdict,
                    "ready_to_propose": ready_to_propose,
                    "kind": kind,
                    "text": text,
                    "environment": {
                        "checked": kind == "assume",
                        "asked_depth": depth,
                        "allowed_steps": env_allowed,
                        "conflict": env_conflict,
                        "conflict_set": conflict_set,
                    },
                    "dead_end": dead_end_val,
                    "overconstraint_warnings": overconstraint_warnings,
                    "candidate_result": candidate_result,
                    "newly_proven": newly_proven,
                    "vacuous_assertions": vacuous_assertions,
                    "lost_covers": lost_covers,
                    "still_failing": still_failing,
                    "target_results": target_results,
                    "cover_results": cover_results,
                    "next_step": next_step,
                }))
            })();

            // Unconditionally clean up scratch task & restore session state.
            let _ = s.set_task(&prev_task);
            let _ = s.delete_task(&scratch);
            s.time_limit = prev_limit;
            s.environment = prev_env;
            s.sink = prev_sink;
            s.assumed_from_assert.truncate(prev_assumed_len);
            outcome
        })?
    }

    pub(super) fn proposal_add(&self, args: &Value) -> Reply {
        self.changing()?;
        let kind = req_str(args, "kind")?;
        let rationale = req_str(args, "rationale")?;
        let name = opt_str(args, "name")?;
        let text = match kind.as_str() {
            "assume" | "assert" | "cover" => req_str(args, "expr")?,
            "cut_point" => req_str(args, "signal")?,
            _ => {
                return Err(err(
                    "arguments",
                    "kind must be assume, assert, cover or cut_point",
                ))
            }
        };
        self.propose(Proposal {
            id: 0,
            kind,
            name,
            text,
            rationale,
            state: "pending".into(),
            error: None,
        })
    }

    /// Check a proposal against the session and record it: applied at once
    /// in automatic mode, pending otherwise. One that the session rejects
    /// is not recorded.
    pub(super) fn propose(&self, mut p: Proposal) -> Reply {
        let auto = self.opts.mode == Mode::Auto;
        let long = p.kind == "command";
        let p = self.on_shell(long, move |sh| {
            if auto {
                apply(sh, &mut p)?;
                p.state = "applied".into();
            } else {
                check(&sh.session.borrow(), &p)?;
            }
            Ok(p)
        })??;
        let mut i = self.state.inner.lock().unwrap();
        i.next_id += 1;
        let p = Proposal { id: i.next_id, ..p };
        i.proposals.push(p.clone());
        Ok(json!({"schema": "informal.proposal/1", "proposal": p}))
    }

    pub(super) fn proposal_list(&self, args: &Value) -> Reply {
        let state = opt_str(args, "state")?;
        // An applied proposal whose property or cut point the session no
        // longer has (`clear -all`, a new elaboration) is gone.
        let applied: Vec<Proposal> = {
            let i = self.state.inner.lock().unwrap();
            i.proposals
                .iter()
                .filter(|p| p.state == "applied")
                .cloned()
                .collect()
        };
        if !applied.is_empty() {
            let gone: Vec<u32> = self.on_shell(false, move |sh| {
                let s = sh.session.borrow();
                applied
                    .iter()
                    .filter(|p| match p.kind.as_str() {
                        "command" => false,
                        "cut_point" => !s.tasks.iter().any(|t| t.ai_stopats.contains(&p.text)),
                        _ => !s
                            .props
                            .iter()
                            .any(|q| q.ai && Some(&q.name) == p.name.as_ref()),
                    })
                    .map(|p| p.id)
                    .collect()
            })?;
            let mut i = self.state.inner.lock().unwrap();
            for p in i.proposals.iter_mut().filter(|p| gone.contains(&p.id)) {
                p.state = "gone".into();
            }
        }
        let i = self.state.inner.lock().unwrap();
        let list: Vec<&Proposal> = i
            .proposals
            .iter()
            .filter(|p| state.as_deref().is_none_or(|s| p.state == s))
            .collect();
        Ok(
            json!({"schema": "informal.proposals/1", "mode": self.opts.mode.label(), "proposals": list}),
        )
    }

    /// Take back one proposal (`id`) or every pending and applied one: a
    /// pending one is withdrawn, an applied one is removed from the
    /// session, and the results found with it in the model are reset.
    pub(super) fn proposal_undo(&self, args: &Value) -> Reply {
        self.changing()?;
        let id = opt_u64(args, "id")?.map(|i| i as u32);
        let targets: Vec<Proposal> = {
            let i = self.state.inner.lock().unwrap();
            let live = |p: &&Proposal| p.state == "pending" || p.state == "applied";
            match id {
                None => i.proposals.iter().filter(live).cloned().collect(),
                Some(id) => {
                    let p = i
                        .proposals
                        .iter()
                        .find(|p| p.id == id)
                        .ok_or_else(|| err("not_found", format!("no proposal {id}")))?;
                    if !live(&p) {
                        return Err(err("rejected", format!("proposal {id} is {}", p.state)));
                    }
                    if p.kind == "command" && p.state == "applied" {
                        return Err(err("rejected", "a Tcl command that ran cannot be undone"));
                    }
                    vec![p.clone()]
                }
            }
        };
        // Last added first, so each removal finds the session as it left it.
        let work: Vec<Proposal> = targets
            .iter()
            .rev()
            .filter(|p| p.state == "applied" && p.kind != "command")
            .cloned()
            .collect();
        let (removed, reset) = self.on_shell(false, move |sh| {
            let mut s = sh.session.borrow_mut();
            let (mut removed, mut reset) = (Vec::new(), Vec::new());
            for p in &work {
                let r = match p.kind.as_str() {
                    "cut_point" => s.remove_ai_cut_point(&p.text),
                    _ => s.remove_ai_property(p.name.as_deref().unwrap_or_default()),
                };
                // Already gone with a cleared session: nothing to remove.
                reset.extend(r.unwrap_or_default());
                removed.push(p.id);
            }
            reset.sort();
            reset.dedup();
            (removed, reset)
        })?;
        let mut i = self.state.inner.lock().unwrap();
        let mut undone = Vec::new();
        for p in i.proposals.iter_mut() {
            if !targets.iter().any(|t| t.id == p.id) {
                continue;
            }
            p.state = match p.state.as_str() {
                "pending" => "withdrawn".into(),
                _ if removed.contains(&p.id) => "undone".into(),
                _ => continue,
            };
            undone.push(p.id);
        }
        Ok(json!({"schema": "informal.undo/1", "undone": undone, "reset": reset}))
    }

    /// The proposals as recorded, for the front end that shows them.
    pub fn proposals(&self) -> Vec<Proposal> {
        self.state.inner.lock().unwrap().proposals.clone()
    }

    /// A person's answer to a pending proposal: apply it, or reject it.
    /// Not a tool: only the front end a person works in calls it.
    pub fn decide(&self, id: u32, accept: bool) -> Reply {
        let mut p = {
            let i = self.state.inner.lock().unwrap();
            let p = i.proposals.iter().find(|p| p.id == id);
            p.cloned()
                .ok_or_else(|| err("not_found", format!("no proposal {id}")))?
        };
        if p.state != "pending" {
            return Err(err("rejected", format!("proposal {id} is {}", p.state)));
        }
        if accept {
            let long = p.kind == "command";
            let _held = if long { Some(self.begin_long()?) } else { None };
            p = self.on_shell(long, move |sh| {
                match apply(sh, &mut p) {
                    Ok(()) => p.state = "applied".into(),
                    Err(e) => {
                        p.state = "failed".into();
                        p.error = Some(e.message);
                    }
                }
                p
            })?;
        } else {
            p.state = "rejected".into();
        }
        let mut i = self.state.inner.lock().unwrap();
        if let Some(q) = i.proposals.iter_mut().find(|q| q.id == id) {
            *q = p.clone();
        }
        drop(i);
        self.changed("decide");
        Ok(json!({"schema": "informal.proposal/1", "proposal": p}))
    }
}

/// What applying `p` would reject, without changing the session.
fn check(s: &Session, p: &Proposal) -> Result<(), ApiError> {
    match p.kind.as_str() {
        "command" => Ok(()),
        "cut_point" => match s.cut_point(&p.text) {
            Ok(cut) if s.stopats_of(&s.current_task).contains(&cut) => {
                Err(err("rejected", format!("`{cut}` is already a cut point"))
                    .with_tool("session_summary"))
            }
            Ok(_) => Ok(()),
            Err(msg) => {
                let sigs = diagnose::all_signal_paths(&s.design.hierarchy);
                let cands = diagnose::closest_matches(&p.text, sigs.iter().map(String::as_str), 3);
                Err(err("rejected", msg)
                    .with_tool("design_hierarchy")
                    .with_hint("Use a signal name from design_hierarchy or property_cone.")
                    .with_candidates(cands))
            }
        },
        _ => s.check_property(p.name.as_deref(), &p.text).map_err(|msg| {
            let sigs = diagnose::all_signal_paths(&s.design.hierarchy);
            let mut cands = Vec::new();
            for id in diagnose::expr_identifiers(&p.text) {
                for m in diagnose::closest_matches(&id, sigs.iter().map(String::as_str), 2) {
                    if !cands.contains(&m) {
                        cands.push(m);
                    }
                }
            }
            cands.truncate(4);
            err("rejected", msg)
                .with_tool("design_hierarchy")
                .with_hint("Check signal names against design_hierarchy and SVA operator syntax.")
                .with_candidates(cands)
        }),
    }
}

/// Make the change, tagged AI-added.
fn apply(sh: &Shell, p: &mut Proposal) -> Result<(), ApiError> {
    let rejected = |e: String| err("rejected", e);
    match p.kind.as_str() {
        "command" => {
            sh.session.borrow().stop.store(false, Ordering::SeqCst);
            sh.eval(&p.text).map_err(|e| rejected(e.message))?;
        }
        "cut_point" => {
            p.text = sh
                .session
                .borrow_mut()
                .add_ai_cut_point(&p.text)
                .map_err(rejected)?;
        }
        kind => {
            let kind = match kind {
                "assume" => Kind::Assume,
                "cover" => Kind::Cover,
                _ => Kind::Assert,
            };
            let mut s = sh.session.borrow_mut();
            p.name = Some(
                s.add_ai_property(kind, p.name.clone(), &p.text)
                    .map_err(rejected)?,
            );
        }
    }
    Ok(())
}

/// A property in a prove reply: without its text and source, which the
/// caller named it by.
fn prove_view(p: &Property) -> Value {
    let mut v = prop_view(p, false);
    if let Some(o) = v.as_object_mut() {
        o.remove("expr");
        o.remove("source");
        o.remove("enabled");
    }
    v
}
