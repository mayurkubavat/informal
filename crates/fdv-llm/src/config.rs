//! `config/llm.toml`: named providers and the default one.
//!
//! ```toml
//! [default]
//! provider = "ollama-gemma"
//!
//! [providers.ollama-gemma]
//! kind = "openai-compat"
//! base_url = "http://localhost:11434/v1"
//! model = "gemma3:12b"
//! timeout_s = 120
//! max_tokens = 512
//! temperature = 0.2
//! ```

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Environment variable that overrides `[default] provider`.
pub const DEFAULT_PROVIDER_ENV: &str = "FDV_LLM_PROVIDER";

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    #[serde(default)]
    pub default: DefaultSection,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefaultSection {
    pub provider: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// `openai-compat` or `fake`.
    pub kind: String,
    /// API root ending in `/v1` (requests go to `{base_url}/chat/completions`).
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// Name of the environment variable holding the API key (never the key
    /// itself). Unset or empty variable: no `Authorization` header.
    pub api_key_env: Option<String>,
    /// Whole-request timeout, streaming included.
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Extra HTTP headers sent with every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// `fake` only: replies given in turn (cycling).
    #[serde(default)]
    pub replies: Vec<String>,
    /// `fake` only: health check fails with this message.
    pub offline: Option<String>,
}

fn default_timeout() -> u64 {
    120
}

impl LlmConfig {
    pub fn parse(text: &str) -> Result<Self> {
        let c: LlmConfig = toml::from_str(text).context("llm config")?;
        for (name, p) in &c.providers {
            if p.kind == "openai-compat" && (p.base_url.is_empty() || p.model.is_empty()) {
                bail!("provider {name}: openai-compat needs base_url and model");
            }
        }
        if let Some(d) = &c.default.provider {
            if !c.providers.contains_key(d) {
                bail!("[default] provider {d:?} is not in [providers]");
            }
        }
        Ok(c)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    /// Provider names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// The provider to start with: `env_override` (the value of
    /// `FDV_LLM_PROVIDER`) when set, else `[default] provider`, else the
    /// first configured one.
    pub fn selected(&self, env_override: Option<&str>) -> Result<String> {
        if let Some(n) = env_override.filter(|n| !n.is_empty()) {
            if !self.providers.contains_key(n) {
                bail!(
                    "{DEFAULT_PROVIDER_ENV}={n}: no such provider (have {})",
                    self.names().join(", ")
                );
            }
            return Ok(n.to_string());
        }
        if let Some(d) = &self.default.provider {
            return Ok(d.clone());
        }
        self.providers
            .keys()
            .next()
            .cloned()
            .context("no LLM providers configured")
    }
}

/// `FDV_LLM_CONFIG`, or the nearest `config/llm.toml` at or above `start`.
pub fn find_config(start: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FDV_LLM_CONFIG") {
        return Some(p.into());
    }
    let mut dir = start.to_path_buf();
    loop {
        let c = dir.join("config/llm.toml");
        if c.is_file() {
            return Some(c);
        }
        if !dir.pop() {
            return None;
        }
    }
}
