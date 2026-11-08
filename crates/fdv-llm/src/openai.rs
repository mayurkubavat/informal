//! The OpenAI chat-completions API with streaming, as served by Ollama
//! (`http://localhost:11434/v1`), llama-server (`http://localhost:8080/v1`),
//! vLLM (`http://localhost:8000/v1`) and OpenAI itself.

use crate::sse::{clip, parse_chunk, SseParser};
use crate::{ChatRequest, ChatResponse, LlmProvider, ModelInfo, ProviderConfig};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub struct OpenAiCompat {
    name: String,
    model: String,
    base_url: String,
    api_key: Option<String>,
    headers: BTreeMap<String, String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    client: reqwest::Client,
}

/// A client with rustls on ring and the given whole-request timeout.
fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    // Ignore the error: another part of the process installed one first.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .build()
        .context("http client")
}

impl OpenAiCompat {
    pub fn new(name: &str, base_url: &str, model: &str) -> Result<Self> {
        Ok(OpenAiCompat {
            name: name.to_string(),
            model: model.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: None,
            headers: BTreeMap::new(),
            max_tokens: None,
            temperature: None,
            client: http_client(Duration::from_secs(120))?,
        })
    }

    /// From a config entry; the API key is read from the environment
    /// variable `api_key_env` names (now, once).
    pub fn from_config(name: &str, cfg: &ProviderConfig) -> Result<Self> {
        let api_key = cfg
            .api_key_env
            .as_deref()
            .and_then(|v| std::env::var(v).ok())
            .filter(|k| !k.is_empty());
        Ok(OpenAiCompat {
            api_key,
            headers: cfg.headers.clone(),
            max_tokens: cfg.max_tokens,
            temperature: cfg.temperature,
            client: http_client(Duration::from_secs(cfg.timeout_s.max(1)))?,
            ..OpenAiCompat::new(name, &cfg.base_url, &cfg.model)?
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.client.request(method, format!("{}{path}", self.base_url));
        if let Some(k) = &self.api_key {
            r = r.bearer_auth(k);
        }
        for (k, v) in &self.headers {
            r = r.header(k, v);
        }
        r
    }

    /// The JSON body for `req`.
    pub fn body(&self, req: &ChatRequest) -> Value {
        let mut b = json!({ "model": self.model, "messages": req.messages, "stream": true });
        if let Some(n) = req.max_tokens.or(self.max_tokens) {
            b["max_tokens"] = n.into();
        }
        if let Some(t) = req.temperature.or(self.temperature) {
            b["temperature"] = json!(t);
        }
        b
    }
}

async fn http_error(resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(String::from))
        .unwrap_or_else(|| clip(text.trim()));
    anyhow!("HTTP {status}: {msg}")
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiCompat {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: ChatRequest, on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send)) -> Result<ChatResponse> {
        let t = Instant::now();
        let resp = self
            .request(reqwest::Method::POST, "/chat/completions")
            .json(&self.body(&req))
            .send()
            .await
            .map_err(|e| anyhow!("{}: {}", self.base_url, short(&e)))?;
        if !resp.status().is_success() {
            return Err(http_error(resp).await);
        }
        let mut out = ChatResponse { model: self.model.clone(), ..Default::default() };
        let is_json = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("application/json"));
        if is_json {
            // A server that ignored `stream: true`: one plain JSON answer.
            let v: Value = resp.json().await.context("chat response")?;
            if let Some(e) = v.get("error") {
                bail!("server error: {}", e["message"].as_str().unwrap_or(&e.to_string()));
            }
            out.text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
            out.finish_reason = v["choices"][0]["finish_reason"].as_str().map(String::from);
            if let Some(m) = v["model"].as_str() {
                out.model = m.to_string();
            }
            out.first_token_ms = Some(t.elapsed().as_millis() as u64);
            on_delta(&out.text);
            out.total_ms = out.first_token_ms.unwrap_or_default();
            return Ok(out);
        }
        let mut parser = SseParser::new();
        let mut stream = resp.bytes_stream();
        let mut handle = |data: String, out: &mut ChatResponse| -> Result<bool> {
            let c = parse_chunk(&data)?;
            if c.done {
                return Ok(true);
            }
            if let Some(m) = c.model {
                out.model = m;
            }
            if c.finish_reason.is_some() {
                out.finish_reason = c.finish_reason;
            }
            if !c.text.is_empty() {
                out.first_token_ms.get_or_insert(t.elapsed().as_millis() as u64);
                out.text.push_str(&c.text);
                on_delta(&c.text);
            }
            Ok(false)
        };
        'read: while let Some(bytes) = stream.next().await {
            let bytes = bytes.map_err(|e| {
                if e.is_timeout() {
                    anyhow!("{}: timed out after {} s", self.base_url, t.elapsed().as_secs())
                } else {
                    anyhow!("{}: {e}", self.base_url)
                }
            })?;
            for data in parser.feed(&bytes) {
                if handle(data, &mut out)? {
                    break 'read;
                }
            }
        }
        if let Some(data) = parser.finish() {
            handle(data, &mut out)?;
        }
        out.total_ms = t.elapsed().as_millis() as u64;
        Ok(out)
    }

    async fn health(&self) -> Result<ModelInfo> {
        let resp = self
            .request(reqwest::Method::GET, "/models")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| anyhow!("{}: {}", self.base_url, short(&e)))?;
        if !resp.status().is_success() {
            return Err(http_error(resp).await);
        }
        let v: Value = resp.json().await.context("model list")?;
        let available: Vec<String> = v["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
            .unwrap_or_default();
        // An empty list means the server does not list models; trust the config.
        if !available.is_empty() && !available.iter().any(|m| m == &self.model) {
            bail!("model {} is not served at {} (it has {})", self.model, self.base_url, available.join(", "));
        }
        Ok(ModelInfo { provider: self.name.clone(), model: self.model.clone(), available })
    }
}

/// The innermost cause of a reqwest error ("Connection refused" rather
/// than "error sending request for url ...").
fn short(e: &reqwest::Error) -> String {
    let mut s: &dyn std::error::Error = e;
    while let Some(inner) = s.source() {
        s = inner;
    }
    if e.is_timeout() {
        "timed out".into()
    } else {
        s.to_string()
    }
}
