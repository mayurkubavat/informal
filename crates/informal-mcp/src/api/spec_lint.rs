use super::{err, opt_bool, opt_f64, opt_str, opt_u64, req_str, wrong, Api, Mode, Proposal, Reply};
use crate::{diagnose, spec};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

impl Api {
    pub(super) fn formal_lint(&self, args: &Value) -> Reply {
        let file_filter = opt_str(args, "file")?;
        let (req, rst) = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let req = informal_svlang::Request {
                files: s.design.files.clone(),
                defines: s.design.defines.clone(),
                incdirs: s.design.incdirs.clone(),
                top: s.design.top.clone(),
                params: s.design.params.clone(),
                black_box_modules: s.design.bbox.clone(),
                ..Default::default()
            };
            Ok((req, s.reset.clone()))
        })??;
        let mut findings = informal_svlang::assist::lint_formal_ready(&req, rst.as_deref())
            .map_err(|e| err("internal", e))?;
        if let Some(ref f) = file_filter {
            findings.retain(|d| d.file == *f || d.file.ends_with(&format!("/{f}")));
        }
        let val = serde_json::to_value(&findings).map_err(|e| err("internal", e.to_string()))?;
        Ok(json!({
            "schema": "informal.lint/1",
            "count": findings.len(),
            "findings": val,
        }))
    }

    pub(super) fn formal_fix_apply(&self, args: &Value) -> Reply {
        self.changing()?;
        let file_arg = req_str(args, "file")?;
        let path = self.below_root(&file_arg)?;
        let start = opt_u64(args, "start")?
            .ok_or_else(|| err("arguments", "`start` byte offset is required"))?
            as usize;
        let end = opt_u64(args, "end")?
            .ok_or_else(|| err("arguments", "`end` byte offset is required"))?
            as usize;
        let replacement = req_str(args, "replacement")?;
        if end < start {
            return Err(err("arguments", "`end` must be >= `start`"));
        }
        if self.opts.mode == Mode::Propose {
            let rationale = opt_str(args, "rationale")?.unwrap_or_else(|| {
                format!(
                    "Apply formal-ready quick-fix in {} ({start}..{end})",
                    path.display()
                )
            });
            return self.propose(Proposal {
                id: 0,
                kind: "command".into(),
                name: None,
                text: format!(
                    "# quick-fix {}:{}..{} -> {}",
                    path.display(),
                    start,
                    end,
                    replacement
                ),
                rationale,
                state: "pending".into(),
                error: None,
            });
        }
        // Verify file is part of the session's design files
        let in_design = self.on_shell(false, {
            let check_path = path.clone();
            move |sh| {
                let s = sh.session.borrow();
                s.design
                    .files
                    .iter()
                    .any(|f| f.canonicalize().ok() == check_path.canonicalize().ok())
            }
        })?;
        if !in_design {
            return Err(err(
                "rejected",
                format!(
                    "{} is not in the elaborated design's file list",
                    path.display()
                ),
            ));
        }
        let mut src = std::fs::read_to_string(&path)
            .map_err(|e| err("path", format!("{}: {e}", path.display())))?;
        if end > src.len() || !src.is_char_boundary(start) || !src.is_char_boundary(end) {
            return Err(err(
                "arguments",
                "byte span is out of bounds or not UTF-8 aligned",
            ));
        }
        src.replace_range(start..end, &replacement);
        std::fs::write(&path, &src).map_err(|e| err("path", format!("{}: {e}", path.display())))?;
        self.on_shell(false, move |sh| {
            let _ = sh.eval("elaborate");
            Ok(())
        })??;
        Ok(json!({
            "schema": "informal.fix/1",
            "applied": true,
            "file": path.display().to_string(),
        }))
    }

    pub(super) fn signal_nav(&self, args: &Value) -> Reply {
        let signal = req_str(args, "signal")?;
        let req = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            Ok(informal_svlang::Request {
                files: s.design.files.clone(),
                defines: s.design.defines.clone(),
                incdirs: s.design.incdirs.clone(),
                top: s.design.top.clone(),
                params: s.design.params.clone(),
                black_box_modules: s.design.bbox.clone(),
                ..Default::default()
            })
        })??;
        let nav = informal_svlang::assist::navigate_signal(&req, &signal)
            .map_err(|e| err("internal", e))?;
        let mut v = serde_json::to_value(&nav).map_err(|e| err("internal", e.to_string()))?;
        v["schema"] = json!("informal.signal_nav/1");
        Ok(v)
    }

    pub(super) fn spec_audit(&self, args: &Value, long: bool) -> Reply {
        let spec_file_arg = opt_str(args, "spec_file")?;
        let inline_spec = match args.get("spec") {
            None | Some(Value::Null) => None,
            Some(v) if v.is_object() => Some(v.clone()),
            Some(_) => return Err(wrong("spec", "a JSON object matching informal.spec/1")),
        };
        if spec_file_arg.is_some() && inline_spec.is_some() {
            return Err(err(
                "arguments",
                "pass either `spec_file` or inline `spec`, not both",
            ));
        }
        let task_arg = opt_str(args, "task")?;
        let check_dead_end = opt_bool(args, "check_dead_end")?.unwrap_or(false);
        let depth = opt_u64(args, "depth")?.unwrap_or(10).clamp(1, 50) as u32;
        let mutation_score = opt_f64(args, "mutation_score")?;
        if let Some(ms) = mutation_score {
            if !(0.0..=100.0).contains(&ms) || ms.is_nan() {
                return Err(wrong("mutation_score", "a percentage between 0 and 100"));
            }
        }

        // Resolve spec document before holding the shell for solver work.
        let (parsed_spec, spec_source) = if let Some(v) = inline_spec {
            let sp = spec::FvpSpec::parse_value(&v).map_err(|m| {
                err("arguments", m).with_hint(
                    "Ensure `schema` is `\"informal.spec/1\"` and requirement IDs are unique.",
                )
            })?;
            (sp, "<inline>".to_string())
        } else {
            let root = self.opts.root.clone();
            let known_dirs: Vec<PathBuf> = self.on_shell(long, |sh| {
                let s = sh.session.borrow();
                let mut dirs = Vec::new();
                for f in &s.design.files {
                    if let Some(p) = f.parent().and_then(|d| d.canonicalize().ok()) {
                        if !dirs.contains(&p) {
                            dirs.push(p);
                        }
                    }
                }
                for prop in &s.props {
                    if let Some(src) = &prop.source {
                        if let Some(p) = Path::new(&src.file)
                            .parent()
                            .and_then(|d| d.canonicalize().ok())
                        {
                            if !dirs.contains(&p) {
                                dirs.push(p);
                            }
                        }
                    }
                }
                dirs
            })?;
            let path = if let Some(raw) = spec_file_arg {
                let named = root.join(&raw);
                match named.canonicalize() {
                    Ok(p)
                        if p.parent()
                            .is_some_and(|d| known_dirs.iter().any(|kd| kd == d)) =>
                    {
                        p
                    }
                    _ => self.below_root(&raw)?,
                }
            } else {
                let root2 = root.clone();
                let found = self.on_shell(long, move |sh| {
                    let s = sh.session.borrow();
                    spec::discover_spec_file(&s, &root2)
                })?;
                found.ok_or_else(|| {
                    err(
                        "not_found",
                        "no `fvp.spec.json` found next to open design files or workspace root",
                    )
                    .with_hint(
                        "Pass `spec_file` (path to an `informal.spec/1` JSON file) or inline `spec`.",
                    )
                })?
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| err("path", format!("{}: {e}", path.display())))?;
            let sp = spec::FvpSpec::parse_str(&text).map_err(|m| {
                err("arguments", format!("{}: {m}", path.display()))
                    .with_hint("Ensure `fvp.spec.json` uses `\"schema\": \"informal.spec/1\"`.")
            })?;
            (sp, path.display().to_string())
        };

        self.on_shell(long, move |sh| {
            let mut s = sh.session.borrow_mut();
            let target_task = match task_arg {
                Some(ref t) => s.task_name(t).map_err(|m| {
                    let cands =
                        diagnose::closest_matches(t, s.tasks.iter().map(|tk| tk.name.as_str()), 4);
                    err("not_found", m)
                        .with_tool("session_summary")
                        .with_candidates(cands)
                })?,
                None => s.current_task.clone(),
            };

            let mut rt = spec::AuditRuntime {
                spec_source,
                task: target_task.clone(),
                mutation_score,
                ..Default::default()
            };

            if check_dead_end && s.elaborated {
                let prev_task = s.current_task.clone();
                let prev_env = s.environment.clone();
                let prev_sink = s.sink.take();
                s.stop.store(false, Ordering::SeqCst);

                let _ = s.set_task(&target_task);
                if let Ok(env) = s.environment_now(depth) {
                    rt.env_conflict = env.conflict();
                    rt.conflict_set = env.conflict_set;
                }
                if !rt.env_conflict {
                    if let Ok(de) = s.dead_end_now(depth.min(10)) {
                        rt.dead_end_checked = true;
                        if let Some(hit) = de {
                            rt.dead_end_found = true;
                            rt.dead_end_depth = Some(hit.depth);
                        }
                    }
                } else {
                    rt.dead_end_checked = true;
                }

                let _ = s.set_task(&prev_task);
                s.environment = prev_env;
                s.sink = prev_sink;
            }

            Ok(spec::audit_session(&s, &parsed_spec, &rt))
        })?
    }
}
