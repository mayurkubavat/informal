//! The AI interface in the app (decision D17, docs/ai-interface.md): the
//! session a person is looking at, served to an outside agent on a local
//! socket. The agent connects with `informal mcp --connect`; what it
//! proposes waits for the person (`ai_decide`), and every call that may
//! have changed the session reaches the window as an "ai-activity" event.

use informal_mcp::{Api, Mode, Options, Proposal, State};
use serde::Serialize;
use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The app's side of the interface.
pub struct Ai {
    pub api: Arc<Api>,
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
) -> Ai {
    let opts = Options {
        mode: mode(),
        root: std::env::current_dir().unwrap_or_default(),
        workdir: workdir.to_path_buf(),
        max_time: Duration::from_secs(300),
    };
    let api = Arc::new(Api::attach(worker, state, opts));
    let (socket, error) = match socket.map(|p| listen(api.clone(), &p).map(|()| p)) {
        None => (None, None),
        Some(Ok(p)) => (Some(p), None),
        Some(Err(e)) => (None, Some(e.to_string())),
    };
    Ai { api, socket, error }
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
        let ai = start(
            worker.clone(),
            state,
            &work,
            Some(base.join("run/app.sock")),
        );
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
        // While a command of the person's has the shell, tools say busy.
        let held = ai.api.state().foreign();
        let busy = ai.api.call("session_summary", &serde_json::json!({}));
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
