//! Non-blocking read-only MCP tool handlers backed by [`SessionSnapshot`].
//!
//! When a long call (`prove`, `session_open`, or a desktop-app Tcl command)
//! occupies the shell thread, read tools serve immediately from the latest
//! published [`SessionSnapshot`] instead of failing with `busy` (decisions D9
//! and D17).

use super::{counts, err, no_trace_err, origin_of, prop_view, Api, ApiError, Mode, Reply, State};
use crate::diagnose as rtl_diag;
use informal_fpv::session::glob;
use informal_fpv::{Kind, Property, SessionSnapshot, Status};
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_SCOPE_SIGNALS: usize = 200;

/// Property trace metadata returned by [`Api::live_trace_target`].
pub(super) struct LiveTraceTarget {
    pub(super) view: Value,
    pub(super) expr: String,
    pub(super) path: String,
    pub(super) model: Option<String>,
    pub(super) clocks: Vec<String>,
}

impl Api {
    /// Non-blocking `session_summary` served from the live [`SessionSnapshot`].
    pub(super) fn live_session_summary(&self) -> Reply {
        let snap = self.worker.snapshot_live();
        Ok(snapshot_summary(&snap, self.opts.mode, &self.state))
    }

    /// Non-blocking `property_list` served from the live [`SessionSnapshot`].
    pub(super) fn live_property_list(
        &self,
        kind: Option<String>,
        status: Option<String>,
        name: Option<String>,
        origin: Option<String>,
        task: Option<String>,
    ) -> Reply {
        let snap = self.worker.snapshot_live();
        if let Some(t) = &task {
            snap.task_name(t).map_err(|e| err("not_found", e))?;
        }
        let props: Vec<Value> = snap
            .props
            .iter()
            .filter(|p| task.as_deref().is_none_or(|t| p.task == t))
            .filter(|p| kind.as_deref().is_none_or(|k| p.kind.label() == k))
            .filter(|p| status.as_deref().is_none_or(|w| p.status.word() == w))
            .filter(|p| name.as_deref().is_none_or(|n| glob(n, &p.name)))
            .filter(|p| origin.as_deref().is_none_or(|o| origin_of(p) == o))
            .map(|p| prop_view(p, false))
            .collect();
        Ok(json!({
            "schema": "informal.properties/1",
            "total": snap.props.len(),
            "properties": props,
        }))
    }

    /// Non-blocking `property_get` served from the live [`SessionSnapshot`].
    pub(super) fn live_property_get(&self, name: &str) -> Reply {
        let snap = self.worker.snapshot_live();
        let idx = find_in_props(&snap.props, name)?;
        Ok(json!({
            "schema": "informal.property/1",
            "property": prop_view(&snap.props[idx], true),
        }))
    }

    /// Non-blocking `design_hierarchy` served from the live [`SessionSnapshot`].
    pub(super) fn live_design_hierarchy(
        &self,
        scope: Option<String>,
        depth: u64,
        signals: bool,
    ) -> Reply {
        let snap = self.worker.snapshot_live();
        if !snap.elaborated {
            return Err(err("rejected", "no design is elaborated"));
        }
        let roots: Vec<&informal_svlang::Scope> = match &scope {
            None => snap.design.hierarchy.iter().collect(),
            Some(path) => {
                let all = snap.design.hierarchy.iter().flat_map(|r| r.walk());
                let found: Vec<_> = all.filter(|sc| &sc.path == path).collect();
                if found.is_empty() {
                    return Err(err("not_found", format!("no scope {path}")));
                }
                found
            }
        };
        Ok(json!({
            "schema": "informal.hierarchy/1",
            "top": snap.design.top,
            "scopes": roots.iter().map(|r| scope_view(r, depth, signals)).collect::<Vec<_>>(),
        }))
    }

    /// Known canonical source files from the live [`SessionSnapshot`].
    pub(super) fn live_known_sources(&self) -> Vec<PathBuf> {
        let snap = self.worker.snapshot_live();
        let sources = snap.props.iter().filter_map(|p| p.source.as_ref());
        snap.design
            .files
            .iter()
            .cloned()
            .chain(sources.map(|l| PathBuf::from(&l.file)))
            .filter_map(|p| p.canonicalize().ok())
            .collect()
    }

    /// Non-blocking `open_workbench` served from the live [`SessionSnapshot`].
    pub(super) fn live_open_workbench(&self, focus: Option<String>, view: Option<String>) -> Reply {
        let snap = self.worker.snapshot_live();
        Ok(json!({
            "schema": "informal.workbench/1",
            "focus_property": focus,
            "view": view,
            "session": snap.session_json.clone(),
        }))
    }

    /// Lookup a property's trace metadata from the live [`SessionSnapshot`] so
    /// `trace_get` works on already-settled properties while a prove runs.
    pub(super) fn live_trace_target(&self, name: &str) -> Result<LiveTraceTarget, ApiError> {
        let snap = self.worker.snapshot_live();
        let i = find_in_props(&snap.props, name)?;
        let p = &snap.props[i];
        let Some(path) = &p.trace else {
            return Err(no_trace_err(p));
        };
        let model = std::fs::read_to_string(snap.workdir.join(format!("prop_{i}.btor2"))).ok();
        let top = snap.design.top.as_deref().unwrap_or_default();
        let clocks: Vec<String> = snap
            .clocks
            .iter()
            .map(|c| format!("{top}.{}", c.signal))
            .collect();
        Ok(LiveTraceTarget {
            view: prop_view(p, false),
            expr: p.expr.clone(),
            path: path.clone(),
            model,
            clocks,
        })
    }

    /// Populate prompt hydration fields from the live [`SessionSnapshot`] when
    /// the shell thread is busy.
    pub(super) fn live_prompt_context(&self) -> Value {
        let snap = self.worker.snapshot_live();
        let top = snap
            .design
            .top
            .as_deref()
            .unwrap_or("(not elaborated)")
            .to_string();
        let clocks = if snap.clocks.is_empty() {
            "none".to_string()
        } else {
            snap.clocks
                .iter()
                .map(|c| format!("{} ({})", c.signal, c.edge))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let reset = snap.reset.clone().unwrap_or_else(|| "none".into());
        let (ins, outs, _) = rtl_diag::top_ports(&snap.design.hierarchy);
        let cex: Vec<String> = snap
            .props
            .iter()
            .filter(|p| matches!(p.status, Status::Cex { .. }))
            .map(|p| p.name.clone())
            .take(12)
            .collect();
        let undet: Vec<String> = snap
            .props
            .iter()
            .filter(|p| matches!(p.status, Status::Undetermined { .. }))
            .map(|p| p.name.clone())
            .take(12)
            .collect();
        json!({
            "top": top,
            "clocks": clocks,
            "reset": reset,
            "inputs": if ins.is_empty() { "(none)".into() } else { ins.join(", ") },
            "outputs": if outs.is_empty() { "(none)".into() } else { outs.join(", ") },
            "cex_properties": if cex.is_empty() { "none".into() } else { cex.join(", ") },
            "undetermined_properties": if undet.is_empty() { "none".into() } else { undet.join(", ") },
        })
    }
}

fn find_in_props(props: &[Property], name: &str) -> Result<usize, ApiError> {
    props.iter().position(|p| p.name == name).ok_or_else(|| {
        let cands = rtl_diag::closest_matches(name, props.iter().map(|p| p.name.as_str()), 3);
        err("not_found", format!("no property named {name}"))
            .with_tool("property_list")
            .with_hint(
                "Call property_list to see valid property names (non-embedded tasks use `task::name`).",
            )
            .with_candidates(cands)
    })
}

fn snapshot_summary(s: &SessionSnapshot, mode: Mode, state: &State) -> Value {
    let of = |k: Kind| s.props.iter().filter(|p| p.kind == k).count();
    let targets = s.props.iter().filter(|p| p.kind != Kind::Assume);
    let i = state.inner.lock().unwrap();
    json!({
        "schema": "informal.summary/1",
        "mode": mode.label(),
        "elaborated": s.elaborated,
        "top": s.design.top,
        "files": s.design.files,
        "clocks": s.clocks,
        "reset": s.reset,
        "cut_points": s.stopats_of(&s.current_task),
        "current_task": s.current_task,
        "tasks": s.tasks.iter().map(|t| json!({
            "name": t.name,
            "properties": s.props.iter().filter(|p| p.task == t.name).count(),
            "cut_points": t.stopats,
        })).collect::<Vec<_>>(),
        "black_boxes": s.design.bbox.iter().chain(&s.design.bbox_instances).collect::<Vec<_>>(),
        "time_limit_s": s.time_limit.as_secs_f64(),
        "engines": s.engine_names,
        "properties": {
            "assert": of(Kind::Assert),
            "assume": of(Kind::Assume),
            "cover": of(Kind::Cover),
            "by_status": counts(targets),
        },
        "ai": {
            "properties": s.props.iter().filter(|p| p.ai).count(),
            "cut_points": s.tasks.iter().map(|t| t.ai_stopats.len()).sum::<usize>(),
            "pending_proposals": i.proposals.iter().filter(|p| p.state == "pending").count(),
        },
    })
}

fn scope_view(sc: &informal_svlang::Scope, depth: u64, signals: bool) -> Value {
    let mut v = json!({"path": sc.path, "instance": sc.name, "module": sc.module});
    if signals {
        v["signals"] = json!(sc
            .signals
            .iter()
            .take(MAX_SCOPE_SIGNALS)
            .collect::<Vec<_>>());
        if sc.signals.len() > MAX_SCOPE_SIGNALS {
            v["signals_omitted"] = json!(sc.signals.len() - MAX_SCOPE_SIGNALS);
        }
    }
    if depth == 0 {
        v["children"] = json!([]);
        if !sc.children.is_empty() {
            v["children_omitted"] = json!(sc.children.len());
        }
    } else {
        v["children"] = sc
            .children
            .iter()
            .map(|c| scope_view(c, depth - 1, signals))
            .collect();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Options;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn read_tools_succeed_nonblockingly_while_long_call_is_busy() {
        let dir = std::env::temp_dir().join(format!("informal-mcp-live-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let api = Arc::new(Api::spawn(
            Vec::new(),
            dir.join("work"),
            Options {
                mode: Mode::Auto,
                root: dir.clone(),
                workdir: dir.join("work"),
                max_time: Duration::from_secs(30),
            },
        ));
        // Seed a property so the snapshot has real content.
        let r = api.call(
            "tcl_eval",
            &json!({"command": "assert -name a_live {x |-> y}"}),
        );
        assert!(r.is_ok(), "{r:?}");

        // Hold a `foreign` lock simulating an active prove on the shell thread.
        let held = api.state().foreign();
        let t0 = Instant::now();
        let sum = api.call("session_summary", &json!({})).unwrap();
        assert_eq!(sum["properties"]["assert"], 1);
        let list = api.call("property_list", &json!({})).unwrap();
        assert_eq!(list["properties"][0]["name"], "a_live");
        let got = api
            .call("property_get", &json!({"name": "a_live"}))
            .unwrap();
        assert_eq!(got["property"]["name"], "a_live");
        let wb = api.call("open_workbench", &json!({})).unwrap();
        assert_eq!(wb["session"]["props"][0]["name"], "a_live");
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "live reads took {:?}",
            t0.elapsed()
        );

        // Mutating / prove calls must still reject with `busy`.
        let err = api.call("prove", &json!({"wait": true})).unwrap_err();
        assert_eq!(err.code, "busy");
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
