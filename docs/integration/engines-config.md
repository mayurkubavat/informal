# Engine & LLM Configuration

InFormal separates its solver portfolio and desktop Ask-mode LLM settings into two version-controlled TOML files under `config/`, plus `INFORMAL_*` environment variables for machine-specific overrides.

---

## 1. External Engine Portfolio (`config/engines.toml`)

In-house engines (`informal-bmc`, `informal-kind`, `informal-ic3`, `informal-kind-ic3`, `informal-klive`, and Bitwuzla's `informal-wbmc` / `informal-wkind`) are compiled into the `informal` binary and register automatically.

External model checkers (`rIC3`, `ABC`, `Pono`, `BtorMC`, or custom solvers) are declared in [`config/engines.toml`](../../config/engines.toml) (passed via `--engines <path>`, defaulting to `config/engines.toml` in the current directory). Missing binaries are skipped automatically unless `INFORMAL_REQUIRE_TOOLS=1` is set.

### Entry Format

```toml
[[engine]]
name = "ric3-ic3"
program = "ric3"
args = ["check", "--ui", "false", "--cex", "{model}", "ic3"]
cex_witness = true
formats = ["aiger", "btor2"]
proves = true
finds_bugs = true
mem_limit_mb = 8000

[engine.patterns]
safe = '(?m)^UNSAT$'
unsafe = '(?m)^SAT$'
```

| Field | Meaning |
| --- | --- |
| `program` / `args` | Executable on `PATH` and argument vector (`{model}` is replaced with the model file path) |
| `formats` | Accepted formats: `"aiger"`, `"btor2"` |
| `proves` / `finds_bugs` | Whether the engine runs on safety proofs and/or bug-finding/cover queries |
| `cex_witness` / `witness_on_stdout` | Parse counterexample witness from engine stdout so no secondary BMC replay is needed |
| `mem_limit_mb` | Per-engine RSS/cgroup cap in MB |
| `[engine.patterns]` | Regexes extracting `safe`, `unsafe`, and optional `depth` when exit codes are not definitive |

> **macOS note:** The multi-process `ric3 portfolio` entry uses `fork()` + Mach ports and hangs on macOS; keep `ric3-ic3` and `ric3-bmc` enabled on macOS and use `ric3` portfolio only on Linux (see comments in [`config/engines.toml`](../../config/engines.toml)).

---

## 2. Desktop Ask-Mode LLM Config (`config/llm.toml`)

[`config/llm.toml`](../../config/llm.toml) configures the read-only LLM assistant inside `InFormal.app`'s **Ask** mode (`crates/informal-llm`). It supports:

- **Local Ollama** (default, e.g., local Gemma / Qwen models over OpenAI-compatible HTTP)
- **OpenAI-compatible endpoints** (base URL + API key env var)
- **Anthropic Messages API**

For full Ask-mode configuration, tool allowlist, and prompt behavior, see [docs/ui/ask-mode.md](../ui/ask-mode.md).

---

## 3. Environment Variables Reference

| Variable | Scope | Effect |
| --- | --- | --- |
| `INFORMAL_TOOLS_DIR` | Build & Runtime | Root directory where `cadical` and `bitwuzla/install` are discovered |
| `INFORMAL_CADICAL_DIR` | Build | Explicit path to CaDiCaL build tree |
| `INFORMAL_BITWUZLA_DIR` | Build | Explicit path to Bitwuzla install prefix |
| `INFORMAL_ISOLATE` | Runtime | Run each in-house engine in its own child process (same as `--isolate`) |
| `INFORMAL_WAVE_VIEWER` | Tcl `visualize` | Waveform viewer command (`surfer`, `gtkwave`, etc.) |
| `INFORMAL_AI_SOCKET` | `InFormal.app` | Override Unix socket path for `informal mcp --connect`, or `off` to disable |
| `INFORMAL_AI_MODE` | `InFormal.app` | Default MCP permission mode (`read-only`, `propose`, `auto`) |
| `INFORMAL_ROOT` | Antigravity Plugin | Workspace root allowed by `session_open` (defaults to `$HOME`) |
| `INFORMAL_REQUIRE_TOOLS` | Test Gate | Fail instead of skipping when optional solvers are missing |

---

## Related Docs

- Third-party solver pins & build steps: [docs/third-party.md](../third-party.md) · [`tools.lock`](../../tools.lock)
- Engine architecture & invariant sharing: [docs/engines.md](../engines.md)
