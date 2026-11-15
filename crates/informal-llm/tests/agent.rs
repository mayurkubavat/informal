//! The tool loop: the text protocol with the scripted provider, native
//! tool calling against a stand-in chat-completions server, and the
//! Messages API adapter against a stand-in for that API (no key, no
//! network beyond the loopback).

use informal_llm::agent::{run, Event, Limits, Made, ToolHost, ToolMode};
use informal_llm::{build, Fake, LlmConfig, Message, ToolSpec};
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A host with one fact to give.
#[derive(Default)]
struct Session(Mutex<Vec<(String, Value)>>);

impl ToolHost for Session {
    fn call(&self, name: &str, arguments: &Value) -> Result<String, String> {
        self.0
            .lock()
            .unwrap()
            .push((name.to_string(), arguments.clone()));
        match name {
            "property_list" => {
                Ok(json!({"properties": [{"name": "a_wrong", "status": "cex"}]}).to_string())
            }
            _ => Err(format!("{name} failed")),
        }
    }
}

fn tools() -> Vec<ToolSpec> {
    ["property_list", "trace_get"]
        .iter()
        .map(|n| ToolSpec {
            name: n.to_string(),
            description: format!("{n} of the session"),
            parameters: json!({"type": "object", "properties": {"status": {"type": "string"}}, "required": []}),
        })
        .collect()
}

/// What the loop reported: text as it streamed, calls and results in order.
#[derive(Default)]
struct Seen {
    text: String,
    steps: Vec<String>,
}

impl Seen {
    fn take(&mut self, e: Event) {
        match e {
            Event::Text(t) => self.text.push_str(t),
            Event::Call { name, arguments } => self.steps.push(format!("{name} {arguments}")),
            Event::Result { name, ok } => self.steps.push(format!("{name} -> {ok}")),
        }
    }
}

#[tokio::test]
async fn text_protocol_calls_tools_and_streams_only_the_answer() {
    let p = Fake::new(vec![
        "{\"tool\": \"property_list\", \"arguments\": {\"status\": \"cex\"}}".into(),
        "Next, the trace.\n```json\n{\"tool\": \"trace_get\"}\n```".into(),
        "{\"tool\": \"nope\"}".into(),
        "a_wrong fails: count reaches 3.".into(),
    ]);
    let host = Session::default();
    let mut seen = Seen::default();
    let start = vec![Message::system("You help."), Message::user("what fails?")];
    let out = run(
        &p,
        ToolMode::Text,
        &tools(),
        &host,
        start,
        Limits::default(),
        &mut |e| seen.take(e),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "a_wrong fails: count reaches 3.");
    // The calls' JSON was never shown as answer text; the words a model
    // put before a call were.
    assert_eq!(seen.text, format!("Next, the trace.\n{}", out.text));
    assert_eq!(
        seen.steps,
        [
            "property_list {\"status\":\"cex\"}",
            "property_list -> true",
            "trace_get {}",
            "trace_get -> false",
            "nope {}",
            "nope -> false",
        ]
    );
    assert_eq!(out.steps, 4);
    assert_eq!(
        out.calls[0],
        Made {
            name: "property_list".into(),
            arguments: json!({"status": "cex"}),
            ok: true
        }
    );
    // A tool the model made up never reaches the host.
    assert_eq!(host.0.lock().unwrap().len(), 2);
    // The model saw the protocol, its own call and the tool's reply.
    let sent = p.seen.lock().unwrap();
    assert!(sent[0].messages[0]
        .content
        .contains("- property_list(status?: string)"));
    assert!(sent[0].tools.is_empty());
    let last = &sent[1].messages;
    assert_eq!(
        last[last.len() - 2].content,
        "{\"tool\": \"property_list\", \"arguments\": {\"status\": \"cex\"}}"
    );
    assert!(last[last.len() - 1]
        .content
        .starts_with("Reply of property_list:\n{\"properties\""));
    assert!(sent[3]
        .messages
        .last()
        .unwrap()
        .content
        .contains("no tool named nope"));
}

#[tokio::test]
async fn the_last_step_must_answer_and_a_plain_reply_needs_no_tools() {
    // A model that only ever calls: at the step limit it is told to answer,
    // and whatever it says then is the answer.
    let p = Fake::new(vec!["{\"tool\": \"property_list\"}".into()]);
    let host = Session::default();
    let limits = Limits {
        max_steps: 3,
        max_result_chars: 20,
    };
    let out = run(
        &p,
        ToolMode::Text,
        &tools(),
        &host,
        vec![Message::user("q")],
        limits,
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!((out.steps, out.calls.len()), (3, 2));
    {
        let sent = p.seen.lock().unwrap();
        let last = sent[2].messages.last().unwrap();
        assert!(last.content.starts_with("Answer now"), "{}", last.content);
        // A long reply is cut for the model.
        let reply = sent[1].messages.last().unwrap();
        assert!(reply.content.contains("(cut: ask for less at a time)"));
    }
    // With tools off, the first reply is the answer, JSON or not.
    let p = Fake::new(vec!["{\"tool\": \"property_list\"}".into()]);
    let out = run(
        &p,
        ToolMode::Off,
        &tools(),
        &host,
        vec![Message::user("q")],
        Limits::default(),
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        (out.steps, out.text.as_str()),
        (1, "{\"tool\": \"property_list\"}")
    );
}

/// An HTTP server on a free port that answers the n-th connection with the
/// n-th body as an event stream, and passes each request back.
async fn serve(bodies: Vec<String>) -> (String, tokio::sync::mpsc::Receiver<String>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", l.local_addr().unwrap());
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        for body in bodies {
            let (mut s, _) = l.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let t = String::from_utf8_lossy(&req).to_string();
                if let Some(h) = t.find("\r\n\r\n") {
                    let len = t[..h]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= h + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&req).to_string()).await;
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            s.write_all(head.as_bytes()).await.unwrap();
            for c in body.as_bytes().chunks(29) {
                s.write_all(c).await.unwrap();
            }
            let _ = s.shutdown().await;
        }
    });
    (url, rx)
}

fn events(list: &[Value]) -> String {
    list.iter().map(|e| format!("data: {e}\n\n")).collect()
}

fn body_of(request: &str) -> Value {
    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn native_tool_calls_over_chat_completions() {
    let delta = |d: Value| json!({"model": "m", "choices": [{"delta": d}]});
    // The call's arguments arrive in pieces, as servers stream them.
    let first = events(&[
        delta(
            json!({"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "property_list", "arguments": ""}}]}),
        ),
        delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"status\":"}}]})),
        delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"cex\"}"}}]})),
        json!({"model": "m", "choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ]) + "data: [DONE]\n\n";
    let second = events(&[
        delta(json!({"content": "a_wrong "})),
        delta(json!({"content": "fails."})),
    ]) + "data: [DONE]\n\n";
    let (url, mut rx) = serve(vec![first, second]).await;
    let cfg = LlmConfig::parse(&format!(
        "[providers.t]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"m\"\ntools = \"native\"\n"
    ))
    .unwrap();
    let p = build("t", &cfg.providers["t"]).unwrap();
    assert_eq!(ToolMode::of(&cfg.providers["t"]), ToolMode::Native);
    let host = Session::default();
    let mut seen = Seen::default();
    let out = run(
        p.as_ref(),
        ToolMode::Native,
        &tools(),
        &host,
        vec![Message::user("what fails?")],
        Limits::default(),
        &mut |e| seen.take(e),
    )
    .await
    .unwrap();
    assert_eq!(
        (out.text.as_str(), seen.text.as_str()),
        ("a_wrong fails.", "a_wrong fails.")
    );
    assert_eq!(
        seen.steps,
        [
            "property_list {\"status\":\"cex\"}",
            "property_list -> true"
        ]
    );
    let one = body_of(&rx.recv().await.unwrap());
    assert_eq!(one["tools"][0]["function"]["name"], "property_list");
    let two = body_of(&rx.recv().await.unwrap());
    let m = two["messages"].as_array().unwrap();
    assert_eq!(
        m[1]["tool_calls"][0]["function"]["arguments"],
        "{\"status\":\"cex\"}"
    );
    assert_eq!(
        (m[2]["role"].as_str(), m[2]["tool_call_id"].as_str()),
        (Some("tool"), Some("c1"))
    );
}

#[tokio::test]
async fn messages_api_adapter_calls_tools_and_replays_the_turn_unchanged() {
    // A turn with a thinking block, some text and two tool calls.
    let first = events(&[
        json!({"type": "message_start", "message": {"model": "claude-test", "content": []}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig=="}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Looking."}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "property_list", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"status\""}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": ": \"cex\"}"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "content_block_start", "index": 3, "content_block": {"type": "tool_use", "id": "toolu_2", "name": "trace_get", "input": {}}}),
        json!({"type": "content_block_stop", "index": 3}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
        json!({"type": "message_stop"}),
    ]);
    let second = events(&[
        json!({"type": "message_start", "message": {"model": "claude-test"}}),
        json!({"type": "ping"}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "a_wrong fails."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
    ]);
    let (url, mut rx) = serve(vec![first, second]).await;
    std::env::set_var("INFORMAL_LLM_TEST_ANTHROPIC_KEY", "sekrit");
    // A configured temperature is not sent: this API version rejects it.
    let cfg = LlmConfig::parse(&format!(
        "[providers.a]\nkind = \"anthropic\"\nbase_url = \"{url}\"\nmodel = \"claude-test\"\napi_key_env = \"INFORMAL_LLM_TEST_ANTHROPIC_KEY\"\ntemperature = 0.2\n"
    ))
    .unwrap();
    assert_eq!(ToolMode::of(&cfg.providers["a"]), ToolMode::Native);
    let p = build("a", &cfg.providers["a"]).unwrap();
    let host = Session::default();
    let mut seen = Seen::default();
    let start = vec![Message::system("You help."), Message::user("what fails?")];
    let out = run(
        p.as_ref(),
        ToolMode::Native,
        &tools(),
        &host,
        start,
        Limits::default(),
        &mut |e| seen.take(e),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "a_wrong fails.");
    assert_eq!(seen.text, "Looking.a_wrong fails.");
    assert_eq!(out.model, "claude-test");
    assert_eq!(
        seen.steps,
        [
            "property_list {\"status\":\"cex\"}",
            "property_list -> true",
            "trace_get {}",
            "trace_get -> false"
        ]
    );
    let one = rx.recv().await.unwrap();
    let head = one.to_ascii_lowercase();
    assert!(head.starts_with("post /v1/messages "), "{one}");
    assert!(
        head.contains("x-api-key: sekrit") && head.contains("anthropic-version: 2026-10-21"),
        "{one}"
    );
    let b = body_of(&one);
    assert_eq!(
        (&b["system"], &b["stream"]),
        (&json!("You help."), &json!(true))
    );
    assert!(b.get("temperature").is_none());
    assert_eq!(
        b["messages"],
        json!([{"role": "user", "content": "what fails?"}])
    );
    assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    // The second request: the turn as it came, thinking block and all,
    // then both results in one user message.
    let b = body_of(&rx.recv().await.unwrap());
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m.len(), 3);
    assert_eq!(
        m[1],
        json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "", "signature": "sig=="},
            {"type": "text", "text": "Looking."},
            {"type": "tool_use", "id": "toolu_1", "name": "property_list", "input": {"status": "cex"}},
            {"type": "tool_use", "id": "toolu_2", "name": "trace_get", "input": {}},
        ]})
    );
    assert_eq!(m[2]["role"], "user");
    let results = m[2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        (&results[0]["type"], &results[0]["tool_use_id"]),
        (&json!("tool_result"), &json!("toolu_1"))
    );
    assert_eq!(results[1]["content"], "trace_get failed");
}

#[tokio::test]
async fn a_declined_request_is_an_error_not_an_answer() {
    let stream = events(&[
        json!({"type": "message_start", "message": {"model": "claude-test"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}}),
    ]);
    let (url, _rx) = serve(vec![stream]).await;
    let cfg = LlmConfig::parse(&format!(
        "[providers.a]\nkind = \"anthropic\"\nbase_url = \"{url}\"\nmodel = \"claude-test\"\n"
    ))
    .unwrap();
    let p = build("a", &cfg.providers["a"]).unwrap();
    let e = run(
        p.as_ref(),
        ToolMode::Native,
        &tools(),
        &Session::default(),
        vec![Message::user("q")],
        Limits::default(),
        &mut |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "the model declined the request");
}
