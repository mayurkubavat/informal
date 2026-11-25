//! A shell on its own thread, for front ends that call from many threads
//! (the desktop app's command handlers, the MCP server, and the async CLI
//! REPL). Tcl interpreters belong to the thread that made them, so mutating
//! commands and Tcl scripts run on that one thread while concurrent readers
//! observe a live [`SessionSnapshot`] without blocking behind an active prove
//! (decisions D9 and D17).
//!
//! Stop ordering: every mutating job (`Eval` / `Source`) is stamped with a
//! monotonically increasing `epoch` when submitted. [`Worker::stop`] records
//! `stopped_epoch >= submitted_epoch` and sets the shared `stop` flag, so a
//! Stop triggered right before a queued prove starts is never swallowed, while
//! any command submitted *after* Stop runs normally.

use crate::compat::{Flavor, Shell};
use crate::session::Session;
use crate::snapshot::{self, SessionSnapshot, SnapshotSlot};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

/// What a front end can ask for.
#[derive(Debug, Clone)]
pub enum Request {
    /// Evaluate Tcl (one console line or a whole script).
    Eval(String),
    /// Source a script file.
    Source(PathBuf),
    /// Switch the global command set.
    Flavor(Flavor),
    /// The current session, for redrawing the property table.
    Snapshot,
}

/// Reply to one request: the Tcl result or error, plus the session after it.
#[derive(Debug, Clone, Serialize)]
pub struct Reply {
    pub ok: bool,
    pub result: String,
    pub session: serde_json::Value,
}

enum Job {
    Call {
        req: Request,
        epoch: u64,
        back: mpsc::Sender<Reply>,
    },
    /// A closure to run on the shell thread ([`Worker::with`]).
    Run(Box<dyn FnOnce(&Shell) + Send>),
}

/// Handle to the shell thread. Cheap to clone; `Send + Sync`.
#[derive(Clone)]
pub struct Worker {
    tx: mpsc::Sender<Job>,
    /// The session's stop flag (`Session::stop`), shared.
    stop: Arc<AtomicBool>,
    /// Monotonic counter stamped on each submitted `Eval` / `Source`.
    submitted_epoch: Arc<AtomicU64>,
    /// Highest submitted epoch covered by a [`Worker::stop`] call.
    stopped_epoch: Arc<AtomicU64>,
    /// True while the shell thread is executing a job.
    running: Arc<AtomicBool>,
    /// Latest immutable read snapshot published by the session.
    snapshot: SnapshotSlot,
}

impl Worker {
    /// Start the thread. `make` builds the session on that thread.
    pub fn spawn(flavor: Flavor, make: impl FnOnce() -> Session + Send + 'static) -> Worker {
        let (tx, rx) = mpsc::channel::<Job>();
        let stop = Arc::new(AtomicBool::new(false));
        let submitted_epoch = Arc::new(AtomicU64::new(0));
        let stopped_epoch = Arc::new(AtomicU64::new(0));
        let running = Arc::new(AtomicBool::new(false));
        let (init_tx, init_rx) = mpsc::channel::<SnapshotSlot>();

        let shared_stop = stop.clone();
        let stopped_ep = stopped_epoch.clone();
        let running_flag = running.clone();
        thread::Builder::new()
            .name("informal-shell".into())
            .spawn(move || {
                let mut session = make();
                session.stop = shared_stop;
                let slot = snapshot::new_slot(&session);
                session.snapshot_slot = Some(slot.clone());
                let _ = init_tx.send(slot);
                let shell = Shell::new(session, flavor);
                for job in rx {
                    running_flag.store(true, Ordering::SeqCst);
                    match job {
                        Job::Call { req, epoch, back } => {
                            let is_cmd = matches!(req, Request::Eval(_) | Request::Source(_));
                            let cancelled_before_start =
                                is_cmd && epoch <= stopped_ep.load(Ordering::SeqCst);
                            let (ok, result) = if cancelled_before_start {
                                shell.session.borrow().stop.store(true, Ordering::SeqCst);
                                (false, "stopped".into())
                            } else {
                                if is_cmd {
                                    shell.session.borrow().stop.store(false, Ordering::SeqCst);
                                }
                                match req {
                                    Request::Eval(s) => split(shell.eval(&s)),
                                    Request::Source(p) => split(shell.source(&p)),
                                    Request::Flavor(f) => {
                                        shell.activate(f);
                                        (true, String::new())
                                    }
                                    Request::Snapshot => (true, String::new()),
                                }
                            };
                            shell.session.borrow().publish_snapshot();
                            let session = shell
                                .session
                                .borrow()
                                .snapshot_slot
                                .as_ref()
                                .and_then(|sl| sl.lock().ok().map(|g| g.session_json.clone()))
                                .unwrap_or_default();
                            running_flag.store(false, Ordering::SeqCst);
                            let _ = back.send(Reply {
                                ok,
                                result,
                                session,
                            });
                        }
                        Job::Run(f) => {
                            f(&shell);
                            shell.session.borrow().publish_snapshot();
                            running_flag.store(false, Ordering::SeqCst);
                        }
                    }
                }
            })
            .expect("spawn shell thread");

        let snapshot = init_rx
            .recv()
            .expect("shell thread exited during initialization");
        Worker {
            tx,
            stop,
            submitted_epoch,
            stopped_epoch,
            running,
            snapshot,
        }
    }

    /// Ask a running or queued prove/command to stop, from any thread, without
    /// queueing behind it. Any `Eval`/`Source` submitted at or before this call
    /// observes the stop even if it had not yet dequeued on the shell thread;
    /// commands submitted *after* this call run normally.
    pub fn stop(&self) {
        let cur = self.submitted_epoch.load(Ordering::SeqCst);
        self.stopped_epoch.fetch_max(cur, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }

    /// True while the shell thread is actively executing a command or closure.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Latest immutable read projection of the session. Never blocks behind an
    /// active prove, elaboration, or script execution.
    pub fn snapshot_live(&self) -> Arc<SessionSnapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Enqueue a Tcl script asynchronously and return a receiver for its reply.
    pub fn eval_async(&self, script: String) -> mpsc::Receiver<Reply> {
        let (tx, rx) = mpsc::channel();
        let epoch = self.submitted_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .tx
            .send(Job::Call {
                req: Request::Eval(script),
                epoch,
                back: tx.clone(),
            })
            .is_err()
        {
            let _ = tx.send(dead());
        }
        rx
    }

    /// Send a request and wait for its reply.
    ///
    /// [`Request::Snapshot`] returns immediately from the live snapshot when
    /// the shell thread is busy so UI/MCP snapshot refreshes never hang behind
    /// a running prove.
    pub fn call(&self, req: Request) -> Reply {
        if matches!(req, Request::Snapshot) && self.is_running() {
            return Reply {
                ok: true,
                result: String::new(),
                session: self.snapshot_live().session_json.clone(),
            };
        }
        let (tx, rx) = mpsc::channel();
        let epoch = if matches!(req, Request::Eval(_) | Request::Source(_)) {
            self.submitted_epoch.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            0
        };
        if self
            .tx
            .send(Job::Call {
                req,
                epoch,
                back: tx,
            })
            .is_err()
        {
            return dead();
        }
        rx.recv().unwrap_or_else(|_| dead())
    }

    /// Run `f` on the shell thread, with the shell, and wait for what it
    /// returns: for front ends that need more of the session than a Tcl
    /// result (the AI interface). `None` when the shell thread has
    /// stopped. Like every mutating request it waits behind a running prove.
    pub fn with<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Shell) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = mpsc::channel();
        let job = Job::Run(Box::new(move |shell| {
            let _ = tx.send(f(shell));
        }));
        self.tx.send(job).ok()?;
        rx.recv().ok()
    }
}

fn split(r: Result<String, informal_tcl::TclError>) -> (bool, String) {
    match r {
        Ok(s) => (true, s),
        Err(e) => (false, e.message),
    }
}

fn dead() -> Reply {
    Reply {
        ok: false,
        result: "the shell thread has stopped".into(),
        session: serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn requests_from_other_threads() {
        let w = Worker::spawn(Flavor::Jg, || {
            Session::new(Vec::new(), std::env::temp_dir().join("informal-worker"))
        });
        let w2 = w.clone();
        let r = thread::spawn(move || w2.call(Request::Eval("assert -name a {x |=> y}".into())))
            .join()
            .unwrap();
        assert!(r.ok, "{}", r.result);
        assert_eq!(r.session["props"][0]["name"], "a");
        assert_eq!(r.session["props"][0]["status"]["status"], "unprocessed");
        let r = w.call(Request::Eval("nonsense".into()));
        assert!(!r.ok && r.result.contains("invalid command name"));
        let r = w.call(Request::Flavor(Flavor::Vcf));
        assert!(r.ok);
        assert!(w.call(Request::Eval("fvcover c -expr {y}".into())).ok);
        // Stop while idle is a no-op for later requests.
        w.stop();
        assert!(w.call(Request::Snapshot).ok);
        // A closure sees the same session.
        let names = w.with(|sh| {
            let s = sh.session.borrow();
            s.props.iter().map(|p| p.name.clone()).collect::<Vec<_>>()
        });
        assert_eq!(names, Some(vec!["a".to_string(), "c".to_string()]));
    }

    #[test]
    fn snapshot_live_and_snapshot_request_do_not_block_during_busy_shell() {
        let w = Worker::spawn(Flavor::Jg, || {
            Session::new(
                Vec::new(),
                std::env::temp_dir().join("informal-worker-live"),
            )
        });
        assert!(w.call(Request::Eval("assert -name a_live {x}".into())).ok);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let w_bg = w.clone();
        let handle = thread::spawn(move || {
            w_bg.with(move |_| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
            });
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(w.is_running());

        let t0 = Instant::now();
        let snap = w.snapshot_live();
        let reply = w.call(Request::Snapshot);
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "live snapshot blocked for {:?}",
            t0.elapsed()
        );
        assert_eq!(snap.props.len(), 1);
        assert_eq!(snap.props[0].name, "a_live");
        assert!(reply.ok);
        assert_eq!(reply.session["props"][0]["name"], "a_live");

        let _ = release_tx.send(());
        handle.join().unwrap();
    }

    #[test]
    fn stop_before_queued_eval_cancels_it_without_affecting_subsequent_eval() {
        let w = Worker::spawn(Flavor::Jg, || {
            Session::new(
                Vec::new(),
                std::env::temp_dir().join("informal-worker-stop-race"),
            )
        });
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let w_hold = w.clone();
        let holder = thread::spawn(move || {
            w_hold.with(move |_| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
            });
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // Queue an eval while shell is busy, then press Stop BEFORE it starts.
        let queued_rx = w.eval_async("assert -name should_not_run {x}".into());
        w.stop();
        // Release the holder: the queued eval must be cancelled, not run!
        let _ = release_tx.send(());
        holder.join().unwrap();
        let queued_reply = queued_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!queued_reply.ok);
        assert_eq!(queued_reply.result, "stopped");
        assert!(w.snapshot_live().props.is_empty());

        // A fresh command submitted AFTER stop must succeed normally.
        let after = w.call(Request::Eval("assert -name after_stop {y}".into()));
        assert!(after.ok, "{}", after.result);
        assert_eq!(w.snapshot_live().props.len(), 1);
        assert_eq!(w.snapshot_live().props[0].name, "after_stop");
    }
}
