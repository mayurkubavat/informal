//! A shell on its own thread, for front ends that call from many threads
//! (the desktop app's command handlers). Tcl interpreters belong to the
//! thread that made them, so every request is sent to that one thread.
//! The one exception is [`Worker::stop`], which must reach a prove that is
//! occupying that thread, so it sets a flag shared with the session instead.
//!
//! TODO(D9, Gemini P9): async request channel so console commands run
//! during a prove, and a shared cancel flag polled in slang lowering,
//! CaDiCaL's terminate callback and Mini (fixes the Stop-before-prove race
//! and lets Stop interrupt elaboration and model building).

use crate::compat::{Flavor, Shell};
use crate::session::Session;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

type Job = (Request, mpsc::Sender<Reply>);

/// Handle to the shell thread. Cheap to clone; `Send + Sync`.
#[derive(Clone)]
pub struct Worker {
    tx: mpsc::Sender<Job>,
    /// The session's stop flag (`Session::stop`), shared.
    stop: Arc<AtomicBool>,
}

impl Worker {
    /// Start the thread. `make` builds the session on that thread.
    pub fn spawn(flavor: Flavor, make: impl FnOnce() -> Session + Send + 'static) -> Worker {
        let (tx, rx) = mpsc::channel::<Job>();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = stop.clone();
        thread::Builder::new()
            .name("informal-shell".into())
            .spawn(move || {
                let mut session = make();
                session.stop = shared;
                let shell = Shell::new(session, flavor);
                for (req, back) in rx {
                    // A Stop pressed while nothing ran must not cancel this
                    // command; one pressed from here on (during elaboration
                    // too) stops its prove.
                    if matches!(req, Request::Eval(_) | Request::Source(_)) {
                        shell.session.borrow().stop.store(false, Ordering::SeqCst);
                    }
                    let (ok, result) = match req {
                        Request::Eval(s) => split(shell.eval(&s)),
                        Request::Source(p) => split(shell.source(&p)),
                        Request::Flavor(f) => {
                            shell.activate(f);
                            (true, String::new())
                        }
                        Request::Snapshot => (true, String::new()),
                    };
                    let session =
                        serde_json::to_value(&*shell.session.borrow()).unwrap_or_default();
                    let _ = back.send(Reply {
                        ok,
                        result,
                        session,
                    });
                }
            })
            .expect("spawn shell thread");
        Worker { tx, stop }
    }

    /// Ask a running prove to stop, from any thread, without queueing
    /// behind it. The prove's own `call` then returns with the session,
    /// unfinished properties undetermined ("stopped"). Harmless when idle:
    /// the next command clears the flag before it starts.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Send a request and wait for its reply.
    pub fn call(&self, req: Request) -> Reply {
        let (tx, rx) = mpsc::channel();
        if self.tx.send((req, tx)).is_err() {
            return dead();
        }
        rx.recv().unwrap_or_else(|_| dead())
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
    }
}
