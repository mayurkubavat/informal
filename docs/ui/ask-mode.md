# Ask mode (console)

The console has two input modes: **Tcl** (commands, as always) and **Ask**
(plain-language questions to an LLM). Switch with the `Tcl | Ask` control at
the far right of the bottom-dock tabs, View > Input mode, or Ctrl+Shift+A.

## Using it

- In Ask mode the prompt reads `ask>`. The answer streams into the console as
  plain `ask:` lines (no bubbles, no colour).
- If the answer contains a Tcl command, the prompt turns back into a Tcl prompt
  (`informal[jg]>`) holding the command, with the hint **Enter runs · Esc discards**.
  You can edit the command before pressing Enter. Enter runs it as a normal Tcl
  command (echoed, logged in Messages). Esc drops it.
- Esc while an answer is streaming cancels it.
- The status bar shows `LLM: <provider>/<model>`, or `LLM: offline` when the
  provider does not answer. Click it to pick another configured provider or to
  check the connection again. While offline, the Ask switch is disabled, and
  its tooltip gives the error.
- Each ask is logged in the Messages tab with its time and the suggested command.

## Safety rule

**Nothing the model says is ever run by itself.** The backend
(`ui/src-tauri/src/main.rs` `ask`) only streams text and returns the first
fenced ```` ```tcl ```` block as `command`. The UI puts that command on the
prompt, and it runs only when the user presses Enter. The model has no tools.
It gets no file access and no shell, only the prompt below.

## What the model is told

`ui/src-tauri/src/ask.rs` `system_prompt` covers:

- the flavor (JasperGold or VC Formal);
- that flavor's command names (`informal_fpv::compat::command_names`, the same list
  Tab completion uses), plus usage examples for the common commands;
- a session summary built from the last reply: top, design file count,
  clocks, reset, the failed assertions and unreachable covers spelled out, and
  up to 200 properties with kind and status;
- rules: answer briefly; return at most one command, in a fenced ```` ```tcl ````
  block; use only the listed commands and existing property names.

The UI adds the selected property as context (`selected property: a_never9`).

## Providers: config/llm.toml

```toml
[default]
provider = "ollama-gemma"

[providers.ollama-gemma]
kind = "openai-compat"          # or "fake"
base_url = "http://localhost:11434/v1"
model = "gemma3:12b"
api_key_env = "SOME_ENV_VAR"    # optional: name of the env var holding the key
timeout_s = 180                 # whole request, streaming included
max_tokens = 512
temperature = 0.2
# [providers.ollama-gemma.headers]   # optional extra HTTP headers
# X-Something = "value"
```

Every `openai-compat` provider is called at `POST {base_url}/chat/completions`
with `stream: true` (server-sent events). The health check is
`GET {base_url}/models`, and the configured model must be in that list when the
server lists any. The shipped file includes:

| name | server | notes |
|---|---|---|
| `ollama-gemma` (default) | Ollama, `gemma3:12b` | local, no key |
| `ollama-gemma-4b` | Ollama, `gemma3:4b` | faster, weaker |
| `fake` | none | scripted reply (`prove -property a_never9`) |
| commented: `llama-server`, `vllm`, `openai`, `anthropic` | | uncomment and set the key's env var |

Anthropic is reached through its OpenAI-compatible endpoint. A native Messages
API adapter is still a TODO in the file.

### Swapping providers

1. **For one run of the app:** click `LLM: …` in the status bar and pick a
   provider. The file is not rewritten.
2. **For one launch:** set `INFORMAL_LLM_PROVIDER=ollama-gemma-4b` (it overrides
   `[default] provider`).
3. **Permanently:** edit `[default] provider`, or add a `[providers.<name>]`
   block for another server.
4. To use another file, set `INFORMAL_LLM_CONFIG=/path/to/llm.toml`.
   Otherwise the nearest `config/llm.toml` above the working directory is used.

### Adding a new kind of API

Implement `informal_llm::LlmProvider` (`crates/informal-llm/src/lib.rs`): `name`,
`model`, a streaming `chat(req, on_delta)`, and `health`. Then add a `kind` arm
in `informal_llm::build`. The app holds providers as `Arc<dyn LlmProvider>`, so
nothing else changes.

## Events (backend ↔ UI)

| command / event | payload |
|---|---|
| `llm_status()` | `{provider, model, ok, error?}` |
| `llm_providers()` | `[{name, kind, model, base_url, selected}]` |
| `llm_select(name)` | switches provider; returns `llm_status` |
| `ask(id, prompt, context?)` | returns at once; `id` is chosen by the UI |
| `"ask-delta"` | `{id, text}` per streamed piece |
| `"ask-done"` | `{id, command?, explanation, error?, model, first_token_ms, total_ms}` |
| `ask_cancel(id)` | aborts the stream; `"ask-done"` follows with `error: "cancelled"` |

## Tests

- `cargo test -p informal-llm` covers config parsing, SSE on a recorded Ollama
  gemma3:12b stream and on a llama-server-style stream (at every chunk size),
  the client against a local stand-in HTTP server, and the Fake provider.
  `cargo test -p informal-llm -- --ignored --nocapture` talks to the real default
  provider.
- `ui/src-tauri` unit tests cover answer splitting, the session summary and
  its 200-property cap, and the prompt contents.
- Vitest: `src/ask.test.ts`, the Ask cases in `Console.test.tsx`, and the mock's
  Fake provider in `mock.test.ts`.
- Playwright: `tests/e2e/ask.spec.ts` (switch, streaming, pre-fill, Enter runs,
  Esc discards, cancel, offline, provider menu, axe), plus the visual baselines
  `ask-suggested`, `ask-offline` and `llm-menu`. In the browser mock,
  `?llm=offline` starts offline, and `ollama-gemma` is always offline.

## Live check (2026-11-08)

Run with `npm run tauri dev`, `tests/fpv/counter/counter.jg.tcl`, and
Ollama `gemma3:12b` on CPU, while another job was using the same model:

- "prove only the a_never9 property": Gemma suggested `prove -property {a_never9}`.
  Enter ran it and a_never9 came back as a CEX. The whole answer took about 2 min
  20 s, almost all of it waiting for the first token (queued behind the other job,
  plus processing about 1.5k prompt tokens on CPU).
- "which properties failed?": "Only the assertion `a_never9` failed with a
  counterexample." It also offered `get_property_list -include {status cex}`.
  Streaming took about 15–20 s once it started. Before the failed/unreachable
  lines were added to the summary, Gemma also listed the unreachable cover c_10
  as a failure.

`gemma3:4b` (`INFORMAL_LLM_PROVIDER=ollama-gemma-4b`) should answer faster when speed
matters more than accuracy.
