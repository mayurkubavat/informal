//! Live prove progress for front ends (the desktop app's "prove-progress"
//! and "console-line" events; see docs/ui/stop-progress.md).
//!
//! [`Session::prove`](crate::Session::prove) drives a [`Tracker`] when the
//! session has a [`ProveSink`]: one event when the run starts, one each time
//! a property starts, reaches a new bound or finishes, a heartbeat while it
//! runs (so elapsed times move), and a final `done` or `stopped`. Every
//! event carries the whole run, so a listener can redraw from any one.

use crate::session::Status;
use informal_engine::portfolio::EngineRun;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One property in a run, as the UI's property table shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropProgress {
    pub name: String,
    /// "assert" or "cover", so a front end can list a property the session
    /// it last saw does not have yet (one added by the same script).
    pub kind: &'static str,
    /// "unprocessed" while queued, "running", then the final status word.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Current k-bound while running; CEX / cover depth when finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Time so far while running; the engine's time when finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Every portfolio engine's run once the property finished (the proof
    /// profiler); empty, and left out, before that.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<EngineRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressKind {
    Start,
    Update,
    Done,
    Stopped,
}

/// Payload of the "prove-progress" event.
#[derive(Debug, Clone, Serialize)]
pub struct ProveProgress {
    pub kind: ProgressKind,
    pub total: usize,
    pub done: usize,
    pub elapsed_ms: u128,
    pub props: Vec<PropProgress>,
}

/// Where progress goes. Called from the shell thread and from engine
/// progress threads, so implementations must be cheap and thread-safe.
pub trait ProveSink: Send + Sync {
    fn progress(&self, p: &ProveProgress);
    /// One console line (a finished property, or the stop notice).
    fn line(&self, _text: &str, _err: bool) {}
    /// One line of an engine's own output while it works on `prop` (the
    /// app's "engine-log" event). External engines stream stdout/stderr
    /// lines as they print them; `err` marks stderr.
    fn engine_log(&self, _line: &EngineLine) {}
}

/// Payload of the "engine-log" event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineLine {
    pub prop: String,
    pub engine: String,
    pub text: String,
    pub err: bool,
}

struct Slot {
    p: PropProgress,
    since: Option<Instant>,
}

/// Progress of one prove run. `slot` is a property's position in the run.
pub struct Tracker {
    sink: Arc<dyn ProveSink>,
    start: Instant,
    slots: Mutex<Vec<Slot>>,
}

fn queued(name: String, kind: &'static str) -> PropProgress {
    PropProgress {
        name,
        kind,
        status: "unprocessed",
        engine: None,
        depth: None,
        ms: None,
        reason: None,
        message: None,
        engines: Vec::new(),
    }
}

/// A finished property's entry, from its session status.
pub fn from_status(name: &str, kind: &'static str, s: &Status) -> PropProgress {
    let mut p = queued(name.to_string(), kind);
    p.status = s.word();
    match s {
        Status::Unprocessed => {}
        Status::Proven { engine, ms } | Status::Unreachable { engine, ms } => {
            p.engine = Some(engine.clone());
            p.ms = Some(*ms);
        }
        Status::Cex { engine, depth, ms } | Status::Covered { engine, depth, ms } => {
            p.engine = Some(engine.clone());
            p.depth = *depth;
            p.ms = Some(*ms);
        }
        Status::Undetermined { reason } => p.reason = Some(reason.clone()),
        Status::Error { message } => p.message = Some(message.clone()),
    }
    p
}

/// The console line for a finished property, e.g.
/// `a_never9: cex at depth 11 [ric3-bmc, 30 ms]`. Same shape as the mock's.
pub fn line_for(p: &PropProgress) -> String {
    let mut s = format!("{}: {}", p.name, p.status);
    if let Some(d) = p.depth {
        s += &format!(" at depth {d}");
    }
    if let Some(r) = &p.reason {
        s += &format!(" ({r})");
    }
    if let Some(m) = &p.message {
        s += &format!(" ({})", m.lines().next().unwrap_or(""));
    }
    if let Some(e) = &p.engine {
        s += &format!(" [{e}, {} ms]", p.ms.unwrap_or(0));
    }
    s
}

fn is_final(p: &PropProgress) -> bool {
    p.status != "running" && p.status != "unprocessed"
}

impl Tracker {
    /// Start a run over `props` (name and kind label, in run order) and
    /// report it.
    pub fn start(
        sink: Arc<dyn ProveSink>,
        props: impl IntoIterator<Item = (String, &'static str)>,
    ) -> Tracker {
        let slots = props
            .into_iter()
            .map(|(n, k)| Slot {
                p: queued(n, k),
                since: None,
            })
            .collect();
        let t = Tracker {
            sink,
            start: Instant::now(),
            slots: Mutex::new(slots),
        };
        t.emit(ProgressKind::Start);
        t
    }

    fn emit(&self, kind: ProgressKind) {
        let props: Vec<PropProgress> = {
            let mut slots = self.slots.lock().unwrap();
            for s in slots.iter_mut() {
                if let (Some(t), "running") = (s.since, s.p.status) {
                    s.p.ms = Some(t.elapsed().as_millis());
                }
            }
            slots.iter().map(|s| s.p.clone()).collect()
        };
        self.sink.progress(&ProveProgress {
            kind,
            total: props.len(),
            done: props.iter().filter(|p| is_final(p)).count(),
            elapsed_ms: self.start.elapsed().as_millis(),
            props,
        });
    }

    /// The property in `slot` is now being proved.
    pub fn running(&self, slot: usize) {
        if let Some(s) = self.slots.lock().unwrap().get_mut(slot) {
            s.p.status = "running";
            s.p.depth = Some(0);
            s.p.ms = Some(0);
            s.since = Some(Instant::now());
        }
        self.emit(ProgressKind::Update);
    }

    /// A line of engine output for the property in `slot`.
    pub fn engine_line(&self, slot: usize, engine: &str, text: String, err: bool) {
        let prop = match self.slots.lock().unwrap().get(slot) {
            Some(s) => s.p.name.clone(),
            None => return,
        };
        self.sink.engine_log(&EngineLine {
            prop,
            engine: engine.to_string(),
            text,
            err,
        });
    }

    /// An engine reported progress on the running property in `slot`. Only
    /// a deeper bound than shown is reported; the engine shown is the one
    /// that reached it.
    pub fn bound(&self, slot: usize, engine: &str, bound: Option<u32>) {
        let Some(b) = bound else { return };
        {
            let mut slots = self.slots.lock().unwrap();
            let Some(s) = slots.get_mut(slot) else { return };
            if s.p.status != "running" || s.p.depth.is_some_and(|d| d >= b) {
                return;
            }
            s.p.depth = Some(b);
            s.p.engine = Some(engine.to_string());
        }
        self.emit(ProgressKind::Update);
    }

    /// The property in `slot` finished with `status`; `engines` is the
    /// portfolio's per-engine record (empty when no engine ran).
    pub fn finished(&self, slot: usize, status: &Status, engines: &[EngineRun]) {
        let line = {
            let mut slots = self.slots.lock().unwrap();
            let Some(s) = slots.get_mut(slot) else { return };
            s.p = from_status(&s.p.name, s.p.kind, status);
            s.p.engines = engines.to_vec();
            s.since = None;
            line_for(&s.p)
        };
        self.sink
            .line(&line, matches!(status, Status::Error { .. }));
        self.emit(ProgressKind::Update);
    }

    /// Heartbeat: re-report so elapsed and running times advance.
    pub fn tick(&self) {
        self.emit(ProgressKind::Update);
    }

    /// End the run. A stopped run reports what had not finished as
    /// undetermined (reason "stopped").
    pub fn end(&self, stopped: bool) {
        if stopped {
            let left = {
                let mut slots = self.slots.lock().unwrap();
                let mut left = 0;
                for s in slots.iter_mut().filter(|s| !is_final(&s.p)) {
                    left += 1;
                    let engine = s.p.engine.take();
                    let ms = s.since.map(|t| t.elapsed().as_millis());
                    s.p = from_status(
                        &s.p.name,
                        s.p.kind,
                        &Status::Undetermined {
                            reason: "stopped".into(),
                        },
                    );
                    s.p.engine = engine;
                    s.p.ms = ms;
                    s.since = None;
                }
                left
            };
            let noun = if left == 1 { "property" } else { "properties" };
            self.sink.line(
                &format!("prove stopped: {left} {noun} left undetermined"),
                true,
            );
        }
        self.emit(if stopped {
            ProgressKind::Stopped
        } else {
            ProgressKind::Done
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        events: Mutex<Vec<ProveProgress>>,
        lines: Mutex<Vec<String>>,
    }
    impl ProveSink for Rec {
        fn progress(&self, p: &ProveProgress) {
            self.events.lock().unwrap().push(p.clone());
        }
        fn line(&self, text: &str, _err: bool) {
            self.lines.lock().unwrap().push(text.into());
        }
    }

    #[test]
    fn reports_start_running_bound_finish_and_done() {
        let rec = Arc::new(Rec::default());
        let t = Tracker::start(
            rec.clone(),
            [("a".to_string(), "assert"), ("b".to_string(), "cover")],
        );
        t.running(0);
        t.bound(0, "bmc", Some(5));
        t.bound(0, "bmc", Some(3)); // not deeper: ignored
        let runs = [EngineRun {
            engine: "bmc".into(),
            outcome: informal_engine::portfolio::RunOutcome::Won,
            ms: 12,
            depth: Some(7),
        }];
        t.finished(
            0,
            &Status::Cex {
                engine: "bmc".into(),
                depth: Some(7),
                ms: 12,
            },
            &runs,
        );
        t.end(false);
        let ev = rec.events.lock().unwrap();
        let kinds: Vec<_> = ev.iter().map(|e| e.kind).collect();
        use ProgressKind::*;
        assert_eq!(kinds, [Start, Update, Update, Update, Done]);
        assert_eq!(ev[0].props[0].status, "unprocessed");
        assert_eq!(ev[1].props[0].status, "running");
        assert_eq!(ev[2].props[0].depth, Some(5));
        assert_eq!(ev[3].props[0].status, "cex");
        assert_eq!(
            (ev[0].props[1].kind, ev[3].props[0].kind),
            ("cover", "assert")
        );
        assert_eq!((ev[4].done, ev[4].total), (1, 2));
        assert_eq!(
            *rec.lines.lock().unwrap(),
            ["a: cex at depth 7 [bmc, 12 ms]"]
        );
        let json = serde_json::to_value(&ev[4]).unwrap();
        assert_eq!(json["kind"], "done");
        assert!(json["props"][1].get("engine").is_none());
        assert_eq!(json["props"][1]["kind"], "cover");
        assert_eq!(json["props"][0]["engines"][0]["outcome"], "won");
        assert!(json["props"][1].get("engines").is_none());
    }

    #[test]
    fn stopping_marks_unfinished_properties() {
        let rec = Arc::new(Rec::default());
        let t = Tracker::start(
            rec.clone(),
            [
                ("a".to_string(), "assert"),
                ("b".to_string(), "assert"),
                ("c".to_string(), "cover"),
            ],
        );
        t.running(0);
        t.finished(
            0,
            &Status::Proven {
                engine: "ic3".into(),
                ms: 3,
            },
            &[],
        );
        t.running(1);
        t.end(true);
        let ev = rec.events.lock().unwrap();
        let last = ev.last().unwrap();
        assert_eq!(last.kind, ProgressKind::Stopped);
        assert_eq!(last.done, 3);
        assert_eq!(last.props[0].status, "proven");
        for p in &last.props[1..] {
            assert_eq!(
                (p.status, p.reason.as_deref()),
                ("undetermined", Some("stopped"))
            );
        }
        assert_eq!(last.props[2].kind, "cover");
        assert_eq!(
            rec.lines.lock().unwrap().last().unwrap(),
            "prove stopped: 2 properties left undetermined"
        );
    }
}
