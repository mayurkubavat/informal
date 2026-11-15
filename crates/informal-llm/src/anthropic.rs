//! The Messages API (`POST {base_url}/messages`, streamed), spoken
//! directly: tool calls arrive as `tool_use` blocks and their results go
//! back as `tool_result` blocks, which the OpenAI-compatible endpoint only
//! approximates.
//!
//! What this adapter holds to:
//! * API version `2026-10-21`, on which sampling parameters are an error:
//!   `temperature` is never sent, whatever the config says.
//! * The assistant's turn goes back exactly as it came ([`Message::raw`]):
//!   thinking blocks are only valid unchanged, in the conversation that
//!   produced them.
//! * `stop_reason` is passed on as the finish reason. `refusal` (the
//!   request was declined) and `max_tokens` are not answers; the caller
//!   decides what to say.
//! * Tool inputs here are small, so they are left buffered: the API then
//!   validates each against its schema before sending it.

use crate::openai::{http_client, http_error, short};
use crate::sse::{clip, SseParser};
use crate::{ChatRequest, ChatResponse, LlmProvider, Message, ModelInfo, ProviderConfig, Role};
use crate::{ToolCall, ToolSpec};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const API_VERSION: &str = "2026-10-21";
const DEFAULT_URL: &str = "https://api.anthropic.com/v1";
/// Without a configured limit: room for an answer, on a streamed request.
const DEFAULT_MAX_TOKENS: u32 = 16_000;

pub struct Anthropic {
    name: String,
    model: String,
    base_url: String,
    api_key: Option<String>,
    max_tokens: u32,
    client: reqwest::Client,
}

impl Anthropic {
    /// From a config entry; the key is read from the environment variable
    /// `api_key_env` names (now, once).
    pub fn from_config(name: &str, cfg: &ProviderConfig) -> Result<Self> {
        let base = match cfg.base_url.is_empty() {
            true => DEFAULT_URL,
            false => cfg.base_url.as_str(),
        };
        Ok(Anthropic {
            name: name.to_string(),
            model: cfg.model.clone(),
            base_url: base.trim_end_matches('/').to_string(),
            api_key: cfg
                .api_key_env
                .as_deref()
                .and_then(|v| std::env::var(v).ok())
                .filter(|k| !k.is_empty()),
            max_tokens: cfg.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            client: http_client(Duration::from_secs(cfg.timeout_s.max(1)))?,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .header("anthropic-version", API_VERSION);
        if let Some(k) = &self.api_key {
            r = r.header("x-api-key", k);
        }
        r
    }

    /// The JSON body for `req`.
    pub fn body(&self, req: &ChatRequest) -> Value {
        let system: Vec<&str> = req
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .collect();
        let mut b = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(self.max_tokens),
            "stream": true,
            "messages": messages(&req.messages),
        });
        if !system.is_empty() {
            b["system"] = json!(system.join("\n\n"));
        }
        if !req.tools.is_empty() {
            b["tools"] = req.tools.iter().map(tool).collect();
        }
        b
    }
}

fn tool(t: &ToolSpec) -> Value {
    json!({"name": t.name, "description": t.description, "input_schema": t.parameters})
}

/// The conversation as the API wants it: no system messages (they are a
/// top-level field), an assistant turn as content blocks, and the results
/// of one turn's tool calls together in the one user message after it.
fn messages(all: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut results_open = false;
    for m in all {
        let was_open = std::mem::replace(&mut results_open, false);
        match m.role {
            Role::System => results_open = was_open,
            Role::User => out.push(json!({"role": "user", "content": m.content})),
            Role::Assistant => {
                let content = match &m.raw {
                    Some(raw) => raw.clone(),
                    None => {
                        let mut blocks = Vec::new();
                        if !m.content.is_empty() {
                            blocks.push(json!({"type": "text", "text": m.content}));
                        }
                        blocks.extend(m.tool_calls.iter().map(|c| {
                            json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments})
                        }));
                        json!(blocks)
                    }
                };
                out.push(json!({"role": "assistant", "content": content}));
            }
            Role::Tool => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id,
                    "content": m.content,
                });
                match (was_open, out.last_mut()) {
                    (true, Some(last)) => last["content"].as_array_mut().unwrap().push(block),
                    _ => out.push(json!({"role": "user", "content": [block]})),
                }
                results_open = true;
            }
        }
    }
    out
}

/// The answer's content blocks, put together from the stream's events.
#[derive(Default)]
struct Blocks {
    blocks: Vec<Value>,
    /// Tool input as JSON text, per block, until the block ends.
    input: Vec<String>,
}

impl Blocks {
    fn at(&mut self, index: usize) -> &mut Value {
        if self.blocks.len() <= index {
            self.blocks.resize(index + 1, Value::Null);
            self.input.resize(index + 1, String::new());
        }
        &mut self.blocks[index]
    }

    /// Take one event. Returns the text it added to the answer, if any.
    fn event(&mut self, v: &Value) -> Result<Option<String>> {
        let index = v["index"].as_u64().unwrap_or(0) as usize;
        match v["type"].as_str().unwrap_or_default() {
            "content_block_start" => *self.at(index) = v["content_block"].clone(),
            "content_block_delta" => {
                let d = &v["delta"];
                let piece = |key: &str| d[key].as_str().unwrap_or_default().to_string();
                let append = |block: &mut Value, key: &str, more: &str| {
                    let old = block[key].as_str().unwrap_or_default();
                    block[key] = json!(format!("{old}{more}"));
                };
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = piece("text");
                        append(self.at(index), "text", &t);
                        return Ok(Some(t));
                    }
                    "input_json_delta" => {
                        self.at(index);
                        self.input[index].push_str(&piece("partial_json"));
                    }
                    "thinking_delta" => append(self.at(index), "thinking", &piece("thinking")),
                    "signature_delta" => self.at(index)["signature"] = d["signature"].clone(),
                    _ => {}
                }
            }
            "content_block_stop" => {
                self.at(index);
                let text = std::mem::take(&mut self.input[index]);
                if self.blocks[index]["type"] == "tool_use" && !text.trim().is_empty() {
                    let name = self.blocks[index]["name"].as_str().unwrap_or_default();
                    self.blocks[index]["input"] = serde_json::from_str(&text).map_err(|e| {
                        anyhow!(
                            "the model's input for {name} is not JSON ({e}): {}",
                            clip(&text)
                        )
                    })?;
                }
            }
            "error" => bail!(
                "server error: {}",
                v["error"]["message"]
                    .as_str()
                    .unwrap_or(&v["error"].to_string())
            ),
            _ => {}
        }
        Ok(None)
    }

    fn tool_calls(&self) -> Vec<ToolCall> {
        self.blocks
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| ToolCall {
                id: b["id"].as_str().unwrap_or_default().to_string(),
                name: b["name"].as_str().unwrap_or_default().to_string(),
                arguments: match &b["input"] {
                    Value::Null => json!({}),
                    v => v.clone(),
                },
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl LlmProvider for Anthropic {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(
        &self,
        req: ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ChatResponse> {
        let t = Instant::now();
        let resp = self
            .request(reqwest::Method::POST, "/messages")
            .json(&self.body(&req))
            .send()
            .await
            .map_err(|e| anyhow!("{}: {}", self.base_url, short(&e)))?;
        if !resp.status().is_success() {
            return Err(http_error(resp).await);
        }
        let mut out = ChatResponse {
            model: self.model.clone(),
            ..Default::default()
        };
        let mut blocks = Blocks::default();
        let mut parser = SseParser::new();
        let mut stream = resp.bytes_stream();
        let mut handle = |data: String, out: &mut ChatResponse| -> Result<()> {
            let v: Value = serde_json::from_str(data.trim())
                .map_err(|e| anyhow!("bad stream event ({e}): {}", clip(&data)))?;
            match v["type"].as_str().unwrap_or_default() {
                "message_start" => {
                    if let Some(m) = v["message"]["model"].as_str() {
                        out.model = m.to_string();
                    }
                }
                "message_delta" => {
                    if let Some(r) = v["delta"]["stop_reason"].as_str() {
                        out.finish_reason = Some(r.to_string());
                    }
                }
                _ => {
                    if let Some(text) = blocks.event(&v)? {
                        out.first_token_ms
                            .get_or_insert(t.elapsed().as_millis() as u64);
                        out.text.push_str(&text);
                        on_delta(&text);
                    }
                }
            }
            Ok(())
        };
        while let Some(bytes) = stream.next().await {
            let bytes = bytes.map_err(|e| match e.is_timeout() {
                true => anyhow!(
                    "{}: timed out after {} s",
                    self.base_url,
                    t.elapsed().as_secs()
                ),
                false => anyhow!("{}: {e}", self.base_url),
            })?;
            for data in parser.feed(&bytes) {
                handle(data, &mut out)?;
            }
        }
        if let Some(data) = parser.finish() {
            handle(data, &mut out)?;
        }
        out.tool_calls = blocks.tool_calls();
        out.raw = Some(json!(blocks.blocks));
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
            .into_iter()
            .flatten()
            .filter_map(|m| m["id"].as_str().map(String::from))
            .collect();
        Ok(ModelInfo {
            provider: self.name.clone(),
            model: self.model.clone(),
            available,
        })
    }
}
