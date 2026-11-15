//! Server-sent events, as OpenAI-style chat streams send them.
//!
//! The byte stream is split into events at blank lines; each event's
//! `data:` lines are joined with `\n`. Chunk boundaries from the network
//! fall anywhere, including inside a UTF-8 character, so bytes are buffered
//! until a whole line is in.

use anyhow::{anyhow, Result};
use serde_json::Value;

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the `data` of every event completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data
                    .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // `event:`, `id:`, `retry:` and `:` comments carry nothing we use.
        }
        out
    }

    /// Whatever event was still open when the stream ended (servers that
    /// omit the final blank line).
    pub fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest)
                .trim_end_matches('\r')
                .to_string();
            if let Some(d) = line.strip_prefix("data:") {
                self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            }
        }
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
    }
}

/// One parsed chat-completion chunk.
#[derive(Debug, Default, PartialEq)]
pub struct Chunk {
    pub text: String,
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    /// `data: [DONE]`.
    pub done: bool,
    /// Pieces of tool calls: a call's id and name come once, its arguments
    /// as fragments of JSON text, all keyed by the call's index.
    pub tool_calls: Vec<ToolDelta>,
}

/// One piece of a streamed tool call.
#[derive(Debug, Default, PartialEq)]
pub struct ToolDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

/// Tool calls put together from their streamed pieces.
#[derive(Default)]
pub struct ToolCalls(Vec<(String, String, String)>);

impl ToolCalls {
    pub fn add(&mut self, d: ToolDelta) {
        if self.0.len() <= d.index {
            self.0.resize(d.index + 1, Default::default());
        }
        let (id, name, args) = &mut self.0[d.index];
        if let Some(i) = d.id {
            *id = i;
        }
        if let Some(n) = d.name {
            *name = n;
        }
        args.push_str(&d.arguments);
    }

    /// Start call `index` with its id and name (arguments follow).
    pub fn start(&mut self, index: usize, id: &str, name: &str) {
        self.add(ToolDelta {
            index,
            id: Some(id.to_string()),
            name: Some(name.to_string()),
            arguments: String::new(),
        });
    }

    /// More of call `index`'s arguments.
    pub fn arguments(&mut self, index: usize, text: &str) {
        self.add(ToolDelta {
            index,
            arguments: text.to_string(),
            ..Default::default()
        });
    }

    /// The calls, in order. Arguments that are not JSON are an error: the
    /// call cannot be made.
    pub fn finish(self) -> Result<Vec<crate::ToolCall>> {
        self.0
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .enumerate()
            .map(|(i, (id, name, args))| {
                let arguments = match args.trim() {
                    "" => serde_json::json!({}),
                    a => serde_json::from_str(a).map_err(|e| {
                        anyhow!(
                            "the model's arguments for {name} are not JSON ({e}): {}",
                            clip(a)
                        )
                    })?,
                };
                Ok(crate::ToolCall {
                    id: if id.is_empty() {
                        format!("call_{i}")
                    } else {
                        id
                    },
                    name,
                    arguments,
                })
            })
            .collect()
    }
}

/// Parse one event's data. An `{"error": ...}` object is an error.
pub fn parse_chunk(data: &str) -> Result<Chunk> {
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(Chunk {
            done: true,
            ..Default::default()
        });
    }
    let v: Value = serde_json::from_str(data)
        .map_err(|e| anyhow!("bad stream chunk ({e}): {}", clip(data)))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| err.to_string());
        return Err(anyhow!("server error: {msg}"));
    }
    let choice = &v["choices"][0];
    Ok(Chunk {
        text: choice["delta"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        model: v["model"].as_str().map(String::from),
        finish_reason: choice["finish_reason"].as_str().map(String::from),
        done: false,
        tool_calls: choice["delta"]["tool_calls"]
            .as_array()
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, c)| ToolDelta {
                        index: c["index"].as_u64().map_or(i, |n| n as usize),
                        id: c["id"].as_str().map(String::from),
                        name: c["function"]["name"].as_str().map(String::from),
                        arguments: c["function"]["arguments"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub(crate) fn clip(s: &str) -> String {
    let mut t: String = s.chars().take(300).collect();
    if t.len() < s.len() {
        t.push('…');
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_arbitrary_chunk_boundaries() {
        let stream =
            "data: {\"a\":1}\n\n: keep-alive\n\ndata: x\r\ndata: y\r\n\r\ndata: [DONE]\n\n";
        for step in 1..stream.len() {
            let mut p = SseParser::new();
            let mut got = Vec::new();
            for c in stream.as_bytes().chunks(step) {
                got.extend(p.feed(c));
            }
            assert_eq!(got, ["{\"a\":1}", "x\ny", "[DONE]"], "chunk size {step}");
            assert_eq!(p.finish(), None);
        }
    }

    #[test]
    fn keeps_utf8_split_inside_a_character() {
        let s = "data: {\"t\":\"µ→\"}\n\n".as_bytes();
        let mut p = SseParser::new();
        let mut got = Vec::new();
        for b in s {
            got.extend(p.feed(std::slice::from_ref(b)));
        }
        assert_eq!(got, ["{\"t\":\"µ→\"}"]);
    }

    #[test]
    fn finish_returns_an_unterminated_event() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: [DONE]").is_empty());
        assert_eq!(p.finish().as_deref(), Some("[DONE]"));
    }

    #[test]
    fn chunk_errors_are_reported() {
        assert!(parse_chunk("{\"error\":{\"message\":\"model not found\"}}")
            .unwrap_err()
            .to_string()
            .contains("model not found"));
        assert!(parse_chunk("not json").is_err());
        assert!(parse_chunk("[DONE]").unwrap().done);
    }
}
