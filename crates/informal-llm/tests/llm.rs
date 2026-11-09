//! informal-llm: config, SSE on recorded streams, the fake, and the
//! OpenAI-compatible client against a local stand-in server.

use informal_llm::{
    build, parse_chunk, ChatRequest, Fake, LlmConfig, LlmProvider, Message, OpenAiCompat, SseParser,
};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn text_of(stream: &[u8], step: usize) -> (String, Option<String>, Option<String>) {
    let mut p = SseParser::new();
    let (mut text, mut model, mut finish) = (String::new(), None, None);
    let mut events = Vec::new();
    for c in stream.chunks(step) {
        events.extend(p.feed(c));
    }
    events.extend(p.finish());
    for e in events {
        let c = parse_chunk(&e).unwrap();
        if c.done {
            break;
        }
        text.push_str(&c.text);
        model = c.model.or(model);
        finish = c.finish_reason.or(finish);
    }
    (text, model, finish)
}

#[test]
fn shipped_config_parses_and_defaults_to_local_gemma() {
    let c = LlmConfig::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/llm.toml"))
        .unwrap();
    assert_eq!(c.selected(None).unwrap(), "ollama-gemma");
    let g = &c.providers["ollama-gemma"];
    assert_eq!(
        (g.kind.as_str(), g.base_url.as_str()),
        ("openai-compat", "http://localhost:11434/v1")
    );
    assert!(g.model.starts_with("gemma"));
    assert_eq!(c.selected(Some("fake")).unwrap(), "fake");
    assert_eq!(c.selected(Some("")).unwrap(), "ollama-gemma");
    assert!(c
        .selected(Some("nope"))
        .unwrap_err()
        .to_string()
        .contains("INFORMAL_LLM_PROVIDER=nope"));
    for (name, p) in &c.providers {
        build(name, p).unwrap();
    }
}

#[test]
fn config_errors_are_caught() {
    let bad_default = "[default]\nprovider = \"x\"\n[providers.y]\nkind = \"fake\"\n";
    assert!(LlmConfig::parse(bad_default)
        .unwrap_err()
        .to_string()
        .contains("\"x\""));
    let no_url = "[providers.y]\nkind = \"openai-compat\"\nmodel = \"m\"\n";
    assert!(LlmConfig::parse(no_url).is_err());
    let typo = "[providers.y]\nkind = \"fake\"\ntemprature = 1\n";
    assert!(LlmConfig::parse(typo).is_err());
    let c = LlmConfig::parse("[providers.z]\nkind = \"carrier-pigeon\"\n").unwrap();
    assert!(build("z", &c.providers["z"]).is_err());
    // No [default]: the first provider, timeout defaulted.
    let c = LlmConfig::parse("[providers.b]\nkind = \"fake\"\n[providers.a]\nkind = \"fake\"\n")
        .unwrap();
    assert_eq!(c.selected(None).unwrap(), "a");
    assert_eq!(c.providers["a"].timeout_s, 120);
    assert!(LlmConfig::parse("").unwrap().selected(None).is_err());
}

#[test]
fn recorded_ollama_stream_parses_at_any_chunking() {
    let s = data("ollama_gemma3_stream.txt");
    let (whole, model, finish) = text_of(&s, s.len());
    assert!(whole.contains("```tcl\n"), "{whole:?}");
    assert_eq!(model.as_deref(), Some("gemma3:12b"));
    assert_eq!(finish.as_deref(), Some("stop"));
    for step in [1, 7, 64, 333] {
        assert_eq!(text_of(&s, step).0, whole, "step {step}");
    }
}

#[test]
fn recorded_llama_server_stream_parses() {
    let (text, model, finish) = text_of(&data("llama_server_stream.txt"), 5);
    assert_eq!(text, "Run:\n```tcl\nprove -property a_never9\n```");
    assert_eq!(model.as_deref(), Some("gemma-3-12b-it"));
    assert_eq!(finish.as_deref(), Some("stop"));
}

#[tokio::test]
async fn fake_streams_its_replies_in_turn() {
    let f = Fake::new(vec!["one two".into(), "three".into()]);
    let mut got = Vec::new();
    let r = f
        .chat(ChatRequest::new(vec![Message::user("hi")]), &mut |d| {
            got.push(d.to_string())
        })
        .await
        .unwrap();
    assert_eq!(
        (r.text.as_str(), got),
        ("one two", vec!["one ".to_string(), "two".into()])
    );
    assert_eq!(
        f.chat(ChatRequest::default(), &mut |_| {})
            .await
            .unwrap()
            .text,
        "three"
    );
    assert_eq!(
        f.chat(ChatRequest::default(), &mut |_| {})
            .await
            .unwrap()
            .text,
        "one two"
    );
    assert_eq!(f.seen.lock().unwrap()[0].messages[0].content, "hi");
    assert!(f.health().await.is_ok());
    let off = Fake::offline("no server");
    assert_eq!(off.health().await.unwrap_err().to_string(), "no server");
    assert!(off.chat(ChatRequest::default(), &mut |_| {}).await.is_err());
}

/// A one-shot HTTP server on a free port: answers each connection with
/// `status`, `ctype` and `body` (written in small pieces), and passes the
/// request it read back over the channel.
async fn serve(
    status: &'static str,
    ctype: &'static str,
    body: Vec<u8>,
    conns: usize,
) -> (String, tokio::sync::mpsc::Receiver<String>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", l.local_addr().unwrap());
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        for _ in 0..conns {
            let (mut s, _) = l.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then Content-Length bytes of body.
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
                format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nConnection: close\r\n\r\n");
            s.write_all(head.as_bytes()).await.unwrap();
            for c in body.chunks(37) {
                s.write_all(c).await.unwrap();
                s.flush().await.unwrap();
            }
            let _ = s.shutdown().await;
        }
    });
    (url, rx)
}

#[tokio::test]
async fn openai_compat_streams_from_a_server() {
    let (url, mut rx) = serve(
        "200 OK",
        "text/event-stream",
        data("llama_server_stream.txt"),
        1,
    )
    .await;
    let mut cfg = LlmConfig::parse(&format!(
        "[providers.t]\nkind = \"openai-compat\"\nbase_url = \"{url}\"\nmodel = \"gemma-3-12b-it\"\napi_key_env = \"INFORMAL_LLM_TEST_KEY\"\nmax_tokens = 77\n[providers.t.headers]\nX-Extra = \"yes\"\n"
    ))
    .unwrap();
    std::env::set_var("INFORMAL_LLM_TEST_KEY", "sekrit");
    let p = build("t", cfg.providers.get_mut("t").unwrap()).unwrap();
    let mut deltas = String::new();
    let r = p
        .chat(
            ChatRequest::new(vec![Message::user("prove a_never9")]),
            &mut |d| deltas.push_str(d),
        )
        .await
        .unwrap();
    assert_eq!(r.text, "Run:\n```tcl\nprove -property a_never9\n```");
    assert_eq!(deltas, r.text);
    assert_eq!(r.finish_reason.as_deref(), Some("stop"));
    assert!(r.first_token_ms.is_some());
    let req = rx.recv().await.unwrap().to_ascii_lowercase();
    assert!(req.starts_with("post /v1/chat/completions "), "{req}");
    assert!(req.contains("authorization: bearer sekrit") && req.contains("x-extra: yes"));
    assert!(
        req.contains("\"stream\":true")
            && req.contains("\"max_tokens\":77")
            && req.contains("prove a_never9")
    );
}

#[tokio::test]
async fn openai_compat_reports_http_errors_and_health() {
    let (url, _rx) = serve(
        "404 Not Found",
        "application/json",
        br#"{"error":{"message":"model \"gemma9\" not found"}}"#.to_vec(),
        1,
    )
    .await;
    let p = OpenAiCompat::new("t", &url, "gemma9").unwrap();
    let e = p
        .chat(ChatRequest::default(), &mut |_| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("404") && e.contains("not found"), "{e}");

    let models = br#"{"object":"list","data":[{"id":"gemma3:12b"},{"id":"gemma3:4b"}]}"#.to_vec();
    let (url, _rx) = serve("200 OK", "application/json", models, 2).await;
    let ok = OpenAiCompat::new("t", &url, "gemma3:4b")
        .unwrap()
        .health()
        .await
        .unwrap();
    assert_eq!(ok.available, ["gemma3:12b", "gemma3:4b"]);
    let e = OpenAiCompat::new("t", &url, "llama9")
        .unwrap()
        .health()
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("llama9 is not served"), "{e}");

    // Nothing listening.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1", l.local_addr().unwrap())
    };
    assert!(OpenAiCompat::new("t", &dead, "m")
        .unwrap()
        .health()
        .await
        .is_err());
}

#[tokio::test]
async fn openai_compat_accepts_a_non_streamed_answer() {
    let body = br#"{"model":"m","choices":[{"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}]}"#.to_vec();
    let (url, _rx) = serve("200 OK", "application/json", body, 1).await;
    let mut got = String::new();
    let r = OpenAiCompat::new("t", &url, "m")
        .unwrap()
        .chat(ChatRequest::default(), &mut |d| got.push_str(d))
        .await
        .unwrap();
    assert_eq!((r.text.as_str(), got.as_str()), ("hello", "hello"));
}

/// Against the configured default provider (the local Ollama Gemma):
/// `cargo test -p informal-llm -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn live_default_provider() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/llm.toml");
    let c = LlmConfig::load(&path).unwrap();
    let name = c
        .selected(std::env::var("INFORMAL_LLM_PROVIDER").ok().as_deref())
        .unwrap();
    let p = build(&name, &c.providers[&name]).unwrap();
    let info = p.health().await.unwrap();
    eprintln!("{name}: {} ({} listed)", info.model, info.available.len());
    let mut deltas = 0;
    let req = ChatRequest {
        messages: vec![
            Message::system("Answer in at most five words."),
            Message::user("Say hello.\nThen say done."),
        ],
        max_tokens: Some(20),
        ..Default::default()
    };
    let r = p.chat(req, &mut |_| deltas += 1).await.unwrap();
    eprintln!(
        "{:?} in {} ms (first token {:?} ms, {deltas} deltas)",
        r.text, r.total_ms, r.first_token_ms
    );
    assert!(!r.text.trim().is_empty());
    assert!(deltas >= 1);
}
