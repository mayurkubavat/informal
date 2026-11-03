//! A shell on its own thread, for front ends that call from many threads
//! (the desktop app's command handlers). Tcl interpreters belong to the
//! thread that made them, so every request is sent to that one thread.

use crate::compat::{Flavor, Shell};
use crate::session::Session;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::mpsc;
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
}

impl Worker {
    /// Start the thread. `make` builds the session on that thread.
    pub fn spawn(flavor: Flavor, make: impl FnOnce() -> Session + Send + 'static) -> Worker {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("fdv-shell".into())
            .spawn(move || {
                let shell = Shell::new(make(), flavor);
                for (req, back) in rx {
                    let (ok, result) = match req {
                        Request::Eval(s) => split(shell.eval(&s)),
                        Request::Source(p) => split(shell.source(&p)),
                        Request::Flavor(f) => {
                            shell.activate(f);
                            (true, String::new())
                        }
                        Request::Snapshot => (true, String::new()),
                    };
                    let session = serde_json::to_value(&*shell.session.borrow()).unwrap_or_default();
                    let _ = back.send(Reply { ok, result, session });
                }
            })
            .expect("spawn shell thread");
        Worker { tx }
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

fn split(r: Result<String, fdv_tcl::TclError>) -> (bool, String) {
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
        let w = Worker::spawn(Flavor::Jg, || Session::new(Vec::new(), std::env::temp_dir().join("fdv-worker")));
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
    }
}
