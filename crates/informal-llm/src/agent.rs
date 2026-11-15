//! A model that looks things up before it answers: the loop that passes a
//! model's tool calls to a [`ToolHost`] and the results back, until the
//! model answers in words.
//!
//! Two ways of giving a model tools ([`ToolMode`]). `Native` uses the
//! API's own tool calling. `Text` is for models served without it (Gemma
//! on Ollama): the tools are described in the system prompt, the model
//! answers a call as one JSON object, and the result goes back as the next
//! user message. The loop runs nothing itself: what a tool may do is the
//! host's business, and the app's host offers only tools that read.

use crate::{ChatRequest, LlmProvider, Message, ProviderConfig, Role, ToolCall, ToolSpec};
use anyhow::{bail, Result};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    Native,
    Text,
    Off,
}

impl ToolMode {
    /// The provider's `tools` setting; without one, native for the
    /// Messages API and text for everything else (an OpenAI-compatible
    /// server may or may not pass tools on to its model).
    pub fn of(cfg: &ProviderConfig) -> ToolMode {
        match (cfg.tools.as_deref(), cfg.kind.as_str()) {
            (Some("native"), _) | (None, "anthropic") => ToolMode::Native,
            (Some("off"), _) => ToolMode::Off,
            _ => ToolMode::Text,
        }
    }
}

/// Runs the tools. `Ok` and `Err` both go back to the model as the tool's
/// reply; `Err` is marked as a failure in [`Event::Result`].
pub trait ToolHost: Send + Sync {
    fn call(&self, name: &str, arguments: &Value) -> Result<String, String>;
}

/// What happens while the loop runs.
pub enum Event<'a> {
    /// A piece of the answer, as it streams.
    Text(&'a str),
    /// The model called a tool.
    Call { name: &'a str, arguments: &'a Value },
    /// The tool returned (`ok`) or failed.
    Result { name: &'a str, ok: bool },
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Model calls at most; the last one must be the answer.
    pub max_steps: usize,
    /// Characters of one tool reply given to the model.
    pub max_result_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_steps: 8,
            max_result_chars: 6000,
        }
    }
}

/// One tool call the loop made.
#[derive(Debug, Clone, PartialEq)]
pub struct Made {
    pub name: String,
    pub arguments: Value,
    pub ok: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// The answer.
    pub text: String,
    pub model: String,
    /// Model calls made.
    pub steps: usize,
    pub calls: Vec<Made>,
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
}

const TEXT_PROTOCOL: &str = "You can look things up with the tools listed below. To call one, \
reply with only a JSON object and nothing else: {\"tool\": \"<name>\", \"arguments\": {...}}. \
You then get the tool's reply and may call another. Base what you say on the replies, never on \
a guess. When you can answer, reply in plain words, not JSON.";

/// One tool for the text protocol: `- name(arg: type, opt?: type): what it does`.
fn describe(t: &ToolSpec) -> String {
    let required: Vec<&str> = t.parameters["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let args: Vec<String> = t.parameters["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, schema)| {
            let kind = match (schema["enum"].as_array(), schema["type"].as_str()) {
                (Some(e), _) => e
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("|"),
                (None, Some("array")) => "list".to_string(),
                (None, t) => t.unwrap_or("any").to_string(),
            };
            let opt = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            format!("{name}{opt}: {kind}")
        })
        .collect();
    format!("- {}({}): {}", t.name, args.join(", "), t.description)
}

/// Where a call may begin in a text-protocol reply: a code fence, a line
/// that opens a JSON object, or `{"tool"` anywhere. Text before it is
/// safe to show.
fn call_start(text: &str) -> Option<usize> {
    let fence = text.find("```");
    let mut at = 0;
    let line = text.split_inclusive('\n').find_map(|line| {
        let here = at;
        at += line.len();
        line.trim_start()
            .starts_with('{')
            .then(|| here + line.len() - line.trim_start().len())
    });
    let inline = text
        .match_indices('{')
        .map(|(i, _)| i)
        .find(|&i| text[i + 1..].trim_start().starts_with("\"tool\""));
    [fence, line, inline].into_iter().flatten().min()
}

/// The call in a text-protocol reply, if there is one, and where it ends:
/// the first JSON object with a `tool` name, alone, in a code fence, or
/// among words. Small models like to announce what they are about to do,
/// and some go on to invent the tool's reply: what follows the call is
/// not theirs to say, and the caller drops it.
fn text_call(text: &str, step: usize) -> Option<(ToolCall, usize)> {
    let from = call_start(text)?;
    let open = from + text[from..].find('{')?;
    let mut values = serde_json::Deserializer::from_str(&text[open..]).into_iter::<Value>();
    let v = values.next()?.ok()?;
    let end = open + values.byte_offset();
    let call = ToolCall {
        id: format!("call_{step}"),
        name: v["tool"].as_str()?.to_string(),
        arguments: match &v["arguments"] {
            Value::Null => serde_json::json!({}),
            a => a.clone(),
        },
    };
    Some((call, end))
}

fn clip(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        s = s.chars().take(max).collect();
        s.push_str(" ... (cut: ask for less at a time)");
    }
    s
}

/// Run the loop on `messages` (system and user messages to start from).
pub async fn run(
    provider: &dyn LlmProvider,
    mode: ToolMode,
    tools: &[ToolSpec],
    host: &dyn ToolHost,
    mut messages: Vec<Message>,
    limits: Limits,
    on_event: &mut (dyn for<'a> FnMut(Event<'a>) + Send),
) -> Result<Outcome> {
    let mode = if tools.is_empty() {
        ToolMode::Off
    } else {
        mode
    };
    if mode == ToolMode::Text {
        let list: Vec<String> = tools.iter().map(describe).collect();
        let protocol = format!("{TEXT_PROTOCOL}\n\nTools:\n{}", list.join("\n"));
        match messages.iter_mut().find(|m| m.role == Role::System) {
            Some(m) => m.content = format!("{}\n\n{protocol}", m.content),
            None => messages.insert(0, Message::system(protocol)),
        }
    }
    let mut out = Outcome::default();
    let steps = limits.max_steps.max(1);
    for step in 1..=steps {
        let last = step == steps;
        if last && step > 1 {
            messages.push(Message::user(
                "Answer now from what you have, in plain words, without calling a tool.",
            ));
        }
        let mut req = ChatRequest::new(messages.clone());
        if mode == ToolMode::Native && !last {
            req.tools = tools.to_vec();
        }
        // In the text protocol, what may be a call (from a code fence or
        // an opening brace on) is held back until the reply is whole, and
        // shown only if it turns out to be part of the answer.
        let holding = mode == ToolMode::Text && !last;
        let (mut full, mut shown) = (String::new(), 0);
        let r = provider
            .chat(req, &mut |d| {
                if !holding {
                    return on_event(Event::Text(d));
                }
                full.push_str(d);
                // Not past a possible call, nor into what may become one.
                let quiet = full.trim_end_matches(|c: char| c == '`' || c.is_whitespace());
                let safe = call_start(&full).unwrap_or(quiet.len()).min(quiet.len());
                if safe > shown {
                    on_event(Event::Text(&full[shown..safe]));
                    shown = safe;
                }
            })
            .await?;
        out.steps = step;
        out.model = r.model.clone();
        out.total_ms += r.total_ms;
        if r.finish_reason.as_deref() == Some("refusal") {
            bail!("the model declined the request");
        }
        // The model's turn as the conversation keeps it: in the text
        // protocol, up to the end of its call.
        let mut turn = r.text.clone();
        let calls = match mode {
            ToolMode::Native => r.tool_calls.clone(),
            ToolMode::Text if !last => match text_call(&r.text, step) {
                Some((call, end)) => {
                    turn.truncate(end);
                    vec![call]
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        if calls.is_empty() {
            if holding && shown < full.len() {
                on_event(Event::Text(&full[shown..]));
            }
            out.first_token_ms = r.first_token_ms.map(|ms| out.total_ms - r.total_ms + ms);
            out.text = r.text;
            return Ok(out);
        }
        messages.push(match mode {
            ToolMode::Native => r.message(),
            _ => Message::assistant(turn),
        });
        for c in calls {
            on_event(Event::Call {
                name: &c.name,
                arguments: &c.arguments,
            });
            let known = tools.iter().any(|t| t.name == c.name);
            let reply = match known {
                true => host.call(&c.name, &c.arguments),
                false => Err(format!("no tool named {}", c.name)),
            };
            let ok = reply.is_ok();
            on_event(Event::Result { name: &c.name, ok });
            let text = clip(reply.unwrap_or_else(|e| e), limits.max_result_chars);
            messages.push(match mode {
                ToolMode::Native => Message::tool_result(c.id, text),
                _ => Message::user(format!("Reply of {}:\n{text}", c.name)),
            });
            out.calls.push(Made {
                name: c.name,
                arguments: c.arguments,
                ok,
            });
        }
    }
    bail!("no answer in {steps} steps")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_text_reply_is_a_call_only_as_a_json_object_with_a_tool() {
        let bare = "{\"tool\": \"property_list\", \"arguments\": {\"status\": \"cex\"}}";
        let (c, end) = text_call(bare, 2).unwrap();
        assert_eq!(
            (c.id.as_str(), c.name.as_str()),
            ("call_2", "property_list")
        );
        assert_eq!((c.arguments, end), (json!({"status": "cex"}), bare.len()));
        let (fenced, _) = text_call("```json\n{\"tool\": \"session_summary\"}\n```", 1).unwrap();
        assert_eq!(fenced.arguments, json!({}));
        // A small model's ways: some words, then the call in a fence...
        let said = "First, the trace.\n```json\n{\"tool\": \"trace_get\", \"arguments\": {\"name\": \"a_wrong\"}}\n```";
        let (c, _) = text_call(said, 1).unwrap();
        assert_eq!(
            (c.name.as_str(), &c.arguments),
            ("trace_get", &json!({"name": "a_wrong"}))
        );
        assert_eq!(call_start(said), Some(18));
        // ... or the call in the middle of a sentence, followed by a reply
        // it made up: the call counts, up to its closing brace.
        let said = "I'll get it: `{\"tool\": \"trace_get\"}`\n\nThe trace shows frame 1: x = 0";
        let (c, end) = text_call(said, 1).unwrap();
        assert_eq!(c.name, "trace_get");
        assert_eq!(&said[..end], "I'll get it: `{\"tool\": \"trace_get\"}");
        assert_eq!(call_start("no call here {not at a line start}"), None);
        assert!(text_call("a_wrong fails because count reaches 3.", 1).is_none());
        assert!(text_call("{\"answer\": 3}", 1).is_none());
    }

    #[test]
    fn tools_are_described_with_their_arguments() {
        let t = ToolSpec {
            name: "trace_get".into(),
            description: "A trace.".into(),
            parameters: json!({"type": "object", "required": ["name"], "properties": {
                "name": {"type": "string"},
                "signals": {"type": "array"},
                "kind": {"type": "string", "enum": ["a", "b"]},
            }}),
        };
        assert_eq!(
            describe(&t),
            "- trace_get(kind?: a|b, name: string, signals?: list): A trace."
        );
    }
}
