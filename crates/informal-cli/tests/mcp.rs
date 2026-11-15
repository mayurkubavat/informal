//! `informal mcp` as a client starts it: a process that speaks the Model
//! Context Protocol on standard input and output. The counter's script
//! prints a report table and its own `puts` lines; none of that may reach
//! the protocol stream. Skipped without CaDiCaL (no in-house engines).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn serves_a_session_on_standard_input_and_output() {
    let work = std::env::temp_dir().join(format!("informal-mcp-cli-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_informal"))
        .current_dir(root())
        // No external engines: the in-house ones settle the counter.
        .args([
            "mcp",
            "--mode",
            "read-only",
            "--engines",
            "no-such-file.toml",
        ])
        .arg("--workdir")
        .arg(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next = 0;
    // Every line on standard output must be a protocol message.
    let mut request = |method: &str, params: Value| -> Value {
        next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": next, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        loop {
            let line = lines.next().expect("the server closed its output").unwrap();
            let m: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("not a protocol message ({e}): {line}"));
            assert_eq!(m["jsonrpc"], "2.0", "{line}");
            if m["id"] == next {
                return m["result"].clone();
            }
        }
    };
    let init = request("initialize", json!({"protocolVersion": "2024-11-05"}));
    assert_eq!(init["protocolVersion"], "2024-11-05");
    assert!(init["instructions"]
        .as_str()
        .unwrap()
        .contains("read-only mode"));
    let tools = request("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"trace_get") && !names.contains(&"proposal_add"),
        "{names:?}"
    );
    let open = request(
        "tools/call",
        json!({"name": "session_open", "arguments": {"script": "tests/fpv/counter/counter.jg.tcl"},
               "_meta": {"progressToken": 7}}),
    );
    let by_status = &open["structuredContent"]["summary"]["properties"]["by_status"];
    if by_status
        .as_object()
        .is_some_and(|o| o.contains_key("error"))
    {
        // Every property in error: this build has no engines.
        let _ = child.kill();
        let _ = child.wait();
        informal_engine::tool_missing("the in-house engines need CaDiCaL");
        return;
    }
    assert_eq!(
        by_status,
        &json!({"cex": 1, "covered": 1, "proven": 4, "unreachable": 1}),
        "{open}"
    );
    let cex = request(
        "tools/call",
        json!({"name": "property_list", "arguments": {"status": "cex"}}),
    );
    assert_eq!(
        cex["structuredContent"]["properties"][0]["name"],
        "a_never9"
    );
    // End of input ends the server.
    drop(request);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    // What the script printed went to standard error.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("PROVEN a_max"), "{err}");
}

/// `--connect`: the process only passes messages between its standard
/// streams and the socket on which a desktop app serves its session.
#[cfg(unix)]
#[test]
fn connect_relays_to_a_served_session() {
    use std::os::unix::net::UnixListener;
    let dir = std::env::temp_dir().join(format!("informal-mcp-relay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("app.sock");
    // The app's side: a session with no engines, served to one client.
    let api = std::sync::Arc::new(informal_mcp::Api::spawn(
        Vec::new(),
        dir.join("work"),
        informal_mcp::Options {
            mode: informal_mcp::Mode::Propose,
            root: root(),
            workdir: dir.join("work"),
            max_time: std::time::Duration::from_secs(5),
        },
    ));
    let listener = UnixListener::bind(&socket).unwrap();
    let served = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let read = BufReader::new(stream.try_clone().unwrap());
        informal_mcp::serve(api, read, stream).unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_informal"))
        .args(["mcp", "--connect"])
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "session_summary", "arguments": {}}})
    )
    .unwrap();
    let reply: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(reply["result"]["structuredContent"]["mode"], "propose");
    // End of input ends the relay and, through it, the served connection.
    drop(stdin);
    assert!(child.wait().unwrap().success());
    served.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
