//! The AI interface in the app (decision D17, docs/ai-interface.md): the
//! session a person is looking at, served to an outside agent on a local
//! socket. The agent connects with `informal mcp --connect`; what it
//! proposes waits for the person (`ai_decide`), and every call that may
//! have changed the session reaches the window as an "ai-activity" event.

use informal_mcp::{Api, Gui, Mode, Options, Proposal, State};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// The window as an agent's `gui_` tools reach it. The views keep `state`
/// current (`ui_state`); an action goes to them as an event and comes back
/// through `done` with what the window then shows, or why it was refused.
pub struct Window {
    state: Mutex<Value>,
    waiting: Mutex<HashMap<u64, Answer>>,
    next: AtomicU64,
    /// Sends one action to the views: `(id, action)`.
    send: ToViews,
}

/// Where the views' answer to one action goes.
type Answer = mpsc::Sender<Result<Value, String>>;
type ToViews = Box<dyn Fn(u64, &Value) + Send + Sync>;

/// How long the views get to do what an agent asked.
const WINDOW_WAIT: Duration = Duration::from_secs(5);

impl Window {
    pub fn new(send: impl Fn(u64, &Value) + Send + Sync + 'static) -> Window {
        Window {
            state: Mutex::new(Value::Null),
            waiting: Mutex::default(),
            next: AtomicU64::new(1),
            send: Box::new(send),
        }
    }

    /// The views report what they show.
    pub fn set_state(&self, state: Value) {
        *self.state.lock().unwrap() = state;
    }

    /// The views report how an action ended.
    pub fn done(&self, id: u64, result: Result<Value, String>) {
        if let Ok(state) = &result {
            self.set_state(state.clone());
        }
        if let Some(tx) = self.waiting.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }
}

impl Gui for Window {
    fn state(&self) -> Result<Value, String> {
        match self.state.lock().unwrap().clone() {
            Value::Null => Err("the window is not up yet".into()),
            s => Ok(s),
        }
    }

    fn act(&self, action: &Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        (self.send)(id, action);
        let r = rx.recv_timeout(WINDOW_WAIT);
        self.waiting.lock().unwrap().remove(&id);
        r.unwrap_or_else(|_| Err("the window did not answer".into()))
    }
}

/// The app's side of the interface.
pub struct Ai {
    pub api: Arc<Api>,
    pub window: Arc<Window>,
    /// Where agents connect; `None` when the socket is off or failed.
    pub socket: Option<PathBuf>,
    /// Why there is no socket, when one was wanted.
    pub error: Option<String>,
}

impl Drop for Ai {
    fn drop(&mut self) {
        if let Some(p) = &self.socket {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// What the window shows of it.
#[derive(Clone, Serialize)]
pub struct AiStatus {
    pub mode: &'static str,
    pub socket: Option<String>,
    pub error: Option<String>,
    pub proposals: Vec<Proposal>,
}

/// `INFORMAL_AI_MODE` (read-only, propose or auto): what an agent may do.
/// Proposing, where every change waits for the person, unless set.
pub fn mode() -> Mode {
    std::env::var("INFORMAL_AI_MODE")
        .ok()
        .and_then(|m| Mode::parse(&m))
        .unwrap_or(Mode::Propose)
}

/// Build the interface over the app's session. With `socket`, listen
/// there: each connection is one agent, served until it hangs up.
pub fn start(
    worker: informal_fpv::Worker,
    state: Arc<State>,
    workdir: &Path,
    socket: Option<PathBuf>,
    window: Window,
) -> Ai {
    let opts = Options {
        mode: mode(),
        root: std::env::current_dir().unwrap_or_default(),
        workdir: workdir.to_path_buf(),
        max_time: Duration::from_secs(300),
    };
    let api = Arc::new(Api::attach(worker, state, opts));
    let window = Arc::new(window);
    api.set_gui(window.clone());
    let (socket, error) = match socket.map(|p| listen(api.clone(), &p).map(|()| p)) {
        None => (None, None),
        Some(Ok(p)) => (Some(p), None),
        Some(Err(e)) => (None, Some(e.to_string())),
    };
    Ai {
        api,
        window,
        socket,
        error,
    }
}

fn listen(api: Arc<Api>, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("informal-ai-socket".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let api = api.clone();
                std::thread::spawn(move || {
                    if let Ok(read) = stream.try_clone() {
                        let _ = informal_mcp::serve(api, BufReader::new(read), stream);
                    }
                });
            }
        })?;
    Ok(())
}

impl Ai {
    pub fn status(&self) -> AiStatus {
        AiStatus {
            mode: self.api.mode().label(),
            socket: self.socket.as_ref().map(|p| p.display().to_string()),
            error: self.error.clone(),
            proposals: self.api.proposals(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_fpv::{Flavor, Session, Worker};
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    /// An agent on the socket shares the app's session: it sees a property
    /// the person added, its proposal waits, and the person's answer puts
    /// it in the session.
    #[test]
    fn an_agent_on_the_socket_shares_the_session() {
        let base = std::env::temp_dir().join(format!("informal-app-ai-{}", std::process::id()));
        let state = Arc::new(State::default());
        let sink = state.clone();
        let work = base.join("work");
        let w = work.clone();
        let worker = Worker::spawn(Flavor::Jg, move || {
            let mut s = Session::new(Vec::new(), w);
            s.sink = Some(sink);
            s
        });
        // A window that answers each action at once with what it was asked.
        let answer: Arc<Mutex<Option<Arc<Window>>>> = Arc::default();
        let views = answer.clone();
        let window = Window::new(move |id, action| {
            if let Some(w) = views.lock().unwrap().as_ref() {
                w.done(
                    id,
                    Ok(serde_json::json!({"view": "trace", "asked": action})),
                );
            }
        });
        let ai = start(
            worker.clone(),
            state,
            &work,
            Some(base.join("run/app.sock")),
            window,
        );
        *answer.lock().unwrap() = Some(ai.window.clone());
        assert_eq!(ai.error, None);
        let path = ai.socket.clone().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // The person types a property at the console.
        assert!(
            worker
                .call(informal_fpv::Request::Eval(
                    "assert -name a_person {x |-> y}".into()
                ))
                .ok
        );
        let changes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = changes.clone();
        ai.api.set_change_hook(Some(Arc::new(move |tool: &str| {
            seen.lock().unwrap().push(tool.to_string())
        })));

        let stream = UnixStream::connect(&path).unwrap();
        let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
        let mut out = stream;
        let mut call = |id: u32, name: &str, args: serde_json::Value| -> serde_json::Value {
            let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": name, "arguments": args}});
            writeln!(out, "{msg}").unwrap();
            let reply: serde_json::Value =
                serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            assert_eq!(reply["id"], id);
            reply["result"]["structuredContent"].clone()
        };
        let list = call(1, "property_list", serde_json::json!({}));
        assert_eq!(list["properties"][0]["name"], "a_person");
        let p = call(
            2,
            "proposal_add",
            serde_json::json!({"kind": "assume", "name": "m_ai", "expr": "x", "rationale": "r"}),
        );
        assert_eq!(p["proposal"]["state"], "pending");
        assert_eq!(ai.status().proposals.len(), 1);
        assert_eq!(ai.status().mode, "propose");
        // The person accepts: the session has it, tagged.
        ai.api.decide(1, true).unwrap();
        let list = call(3, "property_list", serde_json::json!({"origin": "ai"}));
        assert_eq!(list["properties"][0]["name"], "m_ai");
        assert_eq!(*changes.lock().unwrap(), ["proposal_add", "decide"]);
        // The window: nothing to read before the views report, then their
        // state; an action goes to the views and returns what they answer.
        let none = call(4, "gui_state", serde_json::json!({}));
        assert!(none.is_null(), "an error result has no structured content");
        ai.window
            .set_state(serde_json::json!({"selected_property": "a_person", "view": "table"}));
        let seen = call(5, "gui_state", serde_json::json!({}));
        assert_eq!(seen["window"]["selected_property"], "a_person");
        let shown = call(
            6,
            "gui_open_trace",
            serde_json::json!({"name": "a_person", "frame": 3}),
        );
        assert_eq!(shown["window"]["asked"]["action"], "open_trace");
        assert_eq!(shown["window"]["asked"]["frame"], 3);
        assert_eq!(ai.window.state().unwrap()["view"], "trace");
        // While a command of the person's has the shell, read-only queries
        // answer non-blockingly from the live SessionSnapshot while mutating
        // or long commands still answer busy.
        let held = ai.api.state().foreign();
        let live_summary = ai.api.call("session_summary", &serde_json::json!({}));
        assert!(
            live_summary.is_ok(),
            "read-only session_summary should succeed during foreign command: {:?}",
            live_summary
        );
        let live_props = ai.api.call("property_list", &serde_json::json!({}));
        assert!(live_props.is_ok());
        let busy = ai.api.call("prove", &serde_json::json!({"wait": true}));
        assert_eq!(busy.unwrap_err().code, "busy");
        drop(held);
        assert!(ai
            .api
            .call("session_summary", &serde_json::json!({}))
            .is_ok());
        drop(ai);
        assert!(!path.exists(), "the socket is removed with the app");
        let _ = std::fs::remove_dir_all(&base);
    }
}
