//! Swappable LLM providers for the console's Ask mode.
//!
//! Every provider implements [`LlmProvider`]: a streaming chat call that
//! hands each text delta to a callback, and a health check. Two kinds exist:
//!
//! * [`OpenAiCompat`]: `POST {base_url}/chat/completions` with
//!   `stream: true` (server-sent events). Ollama, llama-server, vLLM and
//!   OpenAI all speak it, so swapping the model server is a config change.
//! * [`Fake`]: scripted replies, for tests and the browser mock.
//!
//! Providers are named in `config/llm.toml` ([`LlmConfig`]); `[default]
//! provider` picks one and the `INFORMAL_LLM_PROVIDER` environment variable
//! overrides it. Nothing here runs a command: callers get text back and
//! decide what to do with it.

pub mod agent;
mod anthropic;
mod config;
mod fake;
mod openai;
mod sse;

pub use anthropic::Anthropic;
pub use config::{find_config, LlmConfig, ProviderConfig, DEFAULT_PROVIDER_ENV};
pub use fake::Fake;
pub use openai::OpenAiCompat;
pub use sse::{parse_chunk, Chunk, SseParser};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Who said a chat message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// The result of a tool the assistant called.
    Tool,
}

/// A tool the model may call: its name, what it does, and the JSON Schema
/// of its arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// One call the model asked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call; the result names it.
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Assistant only: the tools it asked for in this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool only: the call this is the result of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Assistant only: the turn in the provider's own form, when it has
    /// parts that must go back unchanged with the tool results (the
    /// Messages API's thinking blocks). Only the provider that made it
    /// reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl Message {
    fn plain(role: Role, s: impl Into<String>) -> Self {
        Message {
            role,
            content: s.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            raw: None,
        }
    }
    pub fn system(s: impl Into<String>) -> Self {
        Message::plain(Role::System, s)
    }
    pub fn user(s: impl Into<String>) -> Self {
        Message::plain(Role::User, s)
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Message::plain(Role::Assistant, s)
    }
    /// The assistant's turn when it called tools (`text` is what it said
    /// beside them, often nothing).
    pub fn calls(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Message {
            tool_calls,
            ..Message::plain(Role::Assistant, text)
        }
    }
    /// What the tool of call `id` returned.
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            tool_call_id: Some(id.into()),
            ..Message::plain(Role::Tool, content)
        }
    }
}

/// One chat call. `max_tokens` / `temperature` left `None` fall back to the
/// provider's configured values.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Tools the model may call. A provider that cannot pass them on
    /// ignores them; [`agent`] then asks for calls in the text.
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        ChatRequest {
            messages,
            ..Default::default()
        }
    }
}

/// The whole answer of a chat call, once the stream ends.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub text: String,
    /// The model the server says answered (may differ from the configured
    /// name, e.g. a resolved tag).
    pub model: String,
    pub finish_reason: Option<String>,
    /// Milliseconds from sending the request to the first text delta.
    pub first_token_ms: Option<u64>,
    /// Milliseconds from sending the request to the end of the stream.
    pub total_ms: u64,
    /// The tools the model asked for instead of, or beside, its text.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// The turn in the provider's own form (see [`Message::raw`]).
    #[serde(default)]
    pub raw: Option<serde_json::Value>,
}

impl ChatResponse {
    /// This answer as the assistant's turn of the conversation.
    pub fn message(&self) -> Message {
        Message {
            raw: self.raw.clone(),
            ..Message::calls(self.text.clone(), self.tool_calls.clone())
        }
    }
}

/// What a health check found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    /// Models the server lists (`GET /models`); empty when it lists none.
    pub available: Vec<String>,
}

/// A chat model behind some API. Object safe, so the app can hold a
/// `Box<dyn LlmProvider>` and swap it at run time.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// The provider's name in the config (e.g. `ollama-gemma`).
    fn name(&self) -> &str;
    /// The model it asks for.
    fn model(&self) -> &str;
    /// Send `req`, calling `on_delta` with each piece of text as it
    /// streams in. Dropping the future cancels the call.
    async fn chat(
        &self,
        req: ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ChatResponse>;
    /// Whether the server answers and serves the model.
    async fn health(&self) -> Result<ModelInfo>;
}

/// Build the provider called `name` from its config entry.
pub fn build(name: &str, cfg: &ProviderConfig) -> Result<Box<dyn LlmProvider>> {
    match cfg.kind.as_str() {
        "openai-compat" => Ok(Box::new(OpenAiCompat::from_config(name, cfg)?)),
        "anthropic" => Ok(Box::new(Anthropic::from_config(name, cfg)?)),
        "fake" => Ok(Box::new(Fake::from_config(name, cfg))),
        other => anyhow::bail!(
            "provider {name}: unknown kind {other:?} (expected \"openai-compat\", \"anthropic\" or \"fake\")"
        ),
    }
}
