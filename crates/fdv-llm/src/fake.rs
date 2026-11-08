//! A provider with scripted replies, for tests and demos.

use crate::{ChatRequest, ChatResponse, LlmProvider, ModelInfo, ProviderConfig};
use anyhow::{bail, Result};
use std::sync::Mutex;
use std::time::Instant;

/// Gives its replies in turn (cycling), streamed a word at a time. Each
/// call is recorded, so tests can check what was sent.
pub struct Fake {
    name: String,
    model: String,
    replies: Vec<String>,
    next: Mutex<usize>,
    offline: Option<String>,
    /// Every request seen, oldest first.
    pub seen: Mutex<Vec<ChatRequest>>,
}

impl Fake {
    pub fn new(replies: Vec<String>) -> Self {
        Fake {
            name: "fake".into(),
            model: "scripted".into(),
            replies,
            next: Mutex::new(0),
            offline: None,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// A fake whose health check fails with `why` (and whose chat errors).
    pub fn offline(why: &str) -> Self {
        Fake { offline: Some(why.to_string()), ..Fake::new(Vec::new()) }
    }

    pub fn from_config(name: &str, cfg: &ProviderConfig) -> Self {
        Fake {
            name: name.to_string(),
            model: if cfg.model.is_empty() { "scripted".into() } else { cfg.model.clone() },
            offline: cfg.offline.clone(),
            ..Fake::new(cfg.replies.clone())
        }
    }
}

/// `text` cut after each run of whitespace: "a b" -> ["a ", "b"].
pub(crate) fn words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_space = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            in_space = true;
        } else if in_space {
            out.push(&text[start..i]);
            start = i;
            in_space = false;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

#[async_trait::async_trait]
impl LlmProvider for Fake {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: ChatRequest, on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send)) -> Result<ChatResponse> {
        if let Some(why) = &self.offline {
            bail!("{why}");
        }
        let t = Instant::now();
        self.seen.lock().unwrap().push(req);
        let text = {
            let mut n = self.next.lock().unwrap();
            let r = if self.replies.is_empty() { String::new() } else { self.replies[*n % self.replies.len()].clone() };
            *n += 1;
            r
        };
        for w in words(&text) {
            on_delta(w);
            tokio::task::yield_now().await;
        }
        Ok(ChatResponse {
            text,
            model: self.model.clone(),
            finish_reason: Some("stop".into()),
            first_token_ms: Some(0),
            total_ms: t.elapsed().as_millis() as u64,
        })
    }

    async fn health(&self) -> Result<ModelInfo> {
        if let Some(why) = &self.offline {
            bail!("{why}");
        }
        Ok(ModelInfo { provider: self.name.clone(), model: self.model.clone(), available: vec![self.model.clone()] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_all_text() {
        let s = "Run this:\n```tcl\nprove -property a\n```";
        assert_eq!(words(s).concat(), s);
        assert_eq!(words("a  b"), ["a  ", "b"]);
        assert!(words("").is_empty());
    }
}
