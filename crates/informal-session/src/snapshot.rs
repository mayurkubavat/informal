//! Lock-free-style concurrent read projection of an active [`Session`].
//!
//! Front ends (MCP tools, desktop app snapshots, async CLI REPL) read from the
//! latest published `Arc<SessionSnapshot>` without waiting for or queueing
//! behind a running `prove`, `elaborate`, or script execution on the Tcl
//! interpreter thread (decisions D9 and D17).

use crate::session::{Clock, Design, Environment, Property, Session, Status, Task};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Shared slot holding the latest published [`SessionSnapshot`].
pub type SnapshotSlot = Arc<Mutex<Arc<SessionSnapshot>>>;

/// Thread-safe, immutable snapshot of a [`Session`]'s queryable state.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub design: Design,
    pub elaborated: bool,
    pub clocks: Vec<Clock>,
    pub reset: Option<String>,
    pub props: Vec<Property>,
    pub tasks: Vec<Task>,
    pub current_task: String,
    pub time_limit: Duration,
    pub engine_names: Vec<String>,
    pub workdir: PathBuf,
    pub environment: Option<Environment>,
    pub hierarchy: Vec<informal_svlang::Instance>,
    pub session_json: serde_json::Value,
}

impl SessionSnapshot {
    /// Build a snapshot from `s`.
    pub fn from_session(s: &Session) -> Self {
        let session_json = serde_json::to_value(s).unwrap_or_default();
        SessionSnapshot {
            design: s.design.clone(),
            elaborated: s.elaborated,
            clocks: s.clocks.clone(),
            reset: s.reset.clone(),
            props: s.props.clone(),
            tasks: s.tasks.clone(),
            current_task: s.current_task.clone(),
            time_limit: s.time_limit,
            engine_names: s.engine_names(),
            workdir: s.workdir.clone(),
            environment: s.environment.clone(),
            hierarchy: s.hierarchy.clone(),
            session_json,
        }
    }

    /// Resolve a task name against the snapshot's task list.
    pub fn task_name(&self, name: &str) -> Result<String, String> {
        self.tasks
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.name.clone())
            .ok_or_else(|| format!("no task named `{name}`"))
    }

    /// Cut points (`stopat`) configured for `task`.
    pub fn stopats_of(&self, task: &str) -> &[String] {
        self.tasks
            .iter()
            .find(|t| t.name == task)
            .map_or(&[], |t| t.stopats.as_slice())
    }

    /// Table rows matching [`Session::report_rows_of`].
    pub fn report_rows_of(&self, task: Option<&str>) -> Vec<[String; 5]> {
        let mut rows = Vec::new();
        for p in self
            .props
            .iter()
            .filter(|p| task.is_none_or(|t| p.task == t))
        {
            let (engine, detail) = match &p.status {
                Status::Proven { engine, ms } | Status::Unreachable { engine, ms } => {
                    (engine.clone(), format!("{:.2} s", *ms as f64 / 1000.0))
                }
                Status::BoundedProven { engine, bound, ms } => (
                    engine.clone(),
                    format!("bound {bound} in {:.2} s", *ms as f64 / 1000.0),
                ),
                Status::Cex { engine, depth, ms } | Status::Covered { engine, depth, ms } => (
                    engine.clone(),
                    format!(
                        "depth {} in {:.2} s",
                        depth.map_or("?".into(), |d| d.to_string()),
                        *ms as f64 / 1000.0
                    ),
                ),
                Status::Undetermined { reason } => (String::new(), reason.clone()),
                Status::Error { message } => {
                    (String::new(), message.lines().next().unwrap_or("").into())
                }
                Status::Unprocessed => (String::new(), String::new()),
            };
            let status = if p.enabled {
                p.status.word()
            } else {
                "disabled"
            };
            let detail = match p.vacuity {
                Some(v) => format!("{detail} [{}]", v.word()),
                None => detail,
            };
            let detail = match (p.ai, p.under_ai.is_empty()) {
                (true, _) => format!("{detail} [AI-added]").trim_start().to_string(),
                (false, false) => format!("{detail} [under AI-added: {}]", p.under_ai.join(", ")),
                (false, true) => detail,
            };
            rows.push([
                p.name.clone(),
                p.kind.label().to_string(),
                status.to_string(),
                engine,
                detail,
            ]);
        }
        rows
    }

    /// JasperGold-style summary table rendered from the snapshot.
    pub fn report_of(&self, task: Option<&str>) -> String {
        let mut s = format!(
            "{:<28} {:<7} {:<13} {:<10} {}\n",
            "property", "type", "status", "engine", "detail"
        );
        for [name, kind, status, engine, detail] in self.report_rows_of(task) {
            s += &format!("{name:<28} {kind:<7} {status:<13} {engine:<10} {detail}\n");
        }
        s
    }

    /// CSV summary table rendered from the snapshot.
    pub fn report_csv_of(&self, task: Option<&str>) -> String {
        let field = |f: &str| {
            if f.contains([',', '"', '\n']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.to_string()
            }
        };
        let mut s = String::from("property,type,status,engine,detail\n");
        for row in self.report_rows_of(task) {
            s += &row.iter().map(|f| field(f)).collect::<Vec<_>>().join(",");
            s.push('\n');
        }
        s
    }
}

/// Create a fresh [`SnapshotSlot`] initialized from `s`.
pub fn new_slot(s: &Session) -> SnapshotSlot {
    Arc::new(Mutex::new(Arc::new(SessionSnapshot::from_session(s))))
}
