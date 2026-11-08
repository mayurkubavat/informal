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
//! provider` picks one and the `FDV_LLM_PROVIDER` environment variable
//! overrides it. Nothing here runs a command: callers get text back and
//! decide what to do with it.

mod config;
mod fake;
mod openai;
mod sse;

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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(s: impl Into<String>) -> Self {
        Message { role: Role::System, content: s.into() }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Message { role: Role::User, content: s.into() }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Message { role: Role::Assistant, content: s.into() }
    }
}

/// One chat call. `max_tokens` / `temperature` left `None` fall back to the
/// provider's configured values.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        ChatRequest { messages, ..Default::default() }
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
    async fn chat(&self, req: ChatRequest, on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send)) -> Result<ChatResponse>;
    /// Whether the server answers and serves the model.
    async fn health(&self) -> Result<ModelInfo>;
}

/// Build the provider called `name` from its config entry.
pub fn build(name: &str, cfg: &ProviderConfig) -> Result<Box<dyn LlmProvider>> {
    match cfg.kind.as_str() {
        "openai-compat" => Ok(Box::new(OpenAiCompat::from_config(name, cfg)?)),
        "fake" => Ok(Box::new(Fake::from_config(name, cfg))),
        other => anyhow::bail!("provider {name}: unknown kind {other:?} (expected \"openai-compat\" or \"fake\")"),
    }
}
