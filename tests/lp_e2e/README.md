# InFormal Low-Power & Core E2E Test Suite (`tests/lp_e2e`)

Opaque-box, requirement-driven end-to-end test suite exercising the compiled `informal` CLI (`shell`, `mcp`, `check`, `lower`, `docgen`), repository scripts, and UI hooks strictly through public interfaces (exit codes, stdout/stderr, JSON reports, NDJSON JSON-RPC 2.0, VCD traces, and Btor2 artifacts).

## Running the Suite

All Python modules in `tests/lp_e2e/` use the Python 3 standard library only and run cleanly in isolated mode (`python3 -I`):

```bash
# Discover cases without executing
python3 -I tests/lp_e2e/run_e2e.py --list

# Validate syntax, stdlib-only imports, TestCase contracts, fixtures, and hygiene
python3 -I tests/lp_e2e/run_e2e.py --validate-only --json

# Run all tiers (defaults --bin to target/debug/informal and --engines to config/engines.toml)
python3 -I tests/lp_e2e/run_e2e.py --tier all

# Filter by tier, id/feature/title/tag substring or glob, and emit JSON report
python3 -I tests/lp_e2e/run_e2e.py --tier 1,2 --filter F4 --json
```

### Runner Flags

| Flag | Description |
|---|---|
| `--tier 1\|2\|3\|4\|all` | Select tiers to run (default: `all`; repeatable or comma-separated). |
| `--bin PATH` | Path to `informal` binary (defaults to `$INFORMAL_BIN` or `target/{debug,release}/informal`). |
| `--engines PATH` | Path to `engines.toml` (defaults to `config/engines.toml`). |
| `--filter PATTERN` | Case-insensitive substring or glob over `id`, `feature`, `title`, and `tags`. |
| `--list` | List matching cases and exit `0`. |
| `--validate-only` | Run `py_compile`, stdlib-only AST check, `TestCase` contract check, fixture checks, and hygiene audit. |
| `--json` | Print machine-readable JSON output on `stdout`. |
| `--timeout SECS` | Scale per-case timeout budget relative to `TestCase.timeout_s`. |
| `--keep-workdir` | Preserve per-case temporary working directories for post-mortem inspection. |
| `--verbose` | Print tracebacks and extra diagnostics on failure. |

The harness automatically prepends `tools/rIC3/target/release` and `tools/abc` to `PATH` and sanitizes stray `INFORMAL_*` environment variables (mirroring `scripts/check.sh`).

## Directory Layout

- `tests/lp_e2e/run_e2e.py` — Opaque-box E2E CLI runner and `--validate-only` gate.
- `tests/lp_e2e/lib/` — Shared infrastructure:
  - `harness.py` — `TestCase`, `E2EContext`, environment setup, and assertion helpers.
  - `cli_client.py` — Process-group-safe subprocess wrappers (`run_shell_file`, `run_shell_commands`, `run_shell_check`, `run_check_model`, `run_lower`, `run_docgen_check`, `run_repo_script`).
  - `mcp_client.py` — NDJSON JSON-RPC 2.0 `McpClient` context manager (`initialize`, `tools_list`, `prompts_list`, `call_tool`, `progress_events`, `close`).
  - `vcd_check.py` — Pure-Python IEEE 1364 VCD parser and miter (`m0.*`/`m1.*`, `__m0`/`__m1`) + `lp.pd.*` trace helpers.
  - `fixtures_gen.py` — Deterministic generators for N-domain UPF/SV, Liberty `.lib`, gate/PG netlists, and behavioral stdcells.
- `tests/lp_e2e/cases/` — Tier 1–4 test modules (`cases/tier*.py`) exporting `get_cases() -> list[TestCase]`.
- `tests/lp_e2e/fixtures/` — Standalone SV, UPF, CPF, Liberty `.lib`, PG netlists, `fvp.spec.json`, and VCD fixtures.

## Tiers & Feature Areas (`F1`–`F10`)

- **Tier 1 (Category-Partition)**: Isolated happy-path coverage (`>= 5` cases per feature `F1`–`F10`).
- **Tier 2 (Boundary & Negative)**: Boundary values, malformed inputs, and error contracts (`>= 5` cases per feature `F1`–`F10`).
- **Tier 3 (Pairwise Combinatorial)**: Cross-feature combinations across `F1`–`F10` (`>= 10` cases).
- **Tier 4 (Real-World Workloads)**: End-to-end SoC verification scenarios (`>= 5` cases).

## Case Authoring Contract

1. Each `tests/lp_e2e/cases/tier*.py` module must export `get_cases() -> list[TestCase]`.
2. Every `TestCase(id, tier, feature, title, fn, timeout_s=60.0, tags=())` must have a unique `id`, `tier` in `{1, 2, 3, 4}`, `feature` in `F1..F10` (or sequence thereof), and `timeout_s > 0`.
3. Use Python standard library imports only (`python3 -I` safe).
4. Write all per-test files into `ctx.workdir` (use `ctx.copy_fixture(...)` before mutating or passing paths to `informal mcp --root`). Never write into tracked repository directories.
5. Pass explicit scripts or `-c` commands to `informal shell` (never pipe raw interactive stdin); use `McpClient` NDJSON over `informal mcp`.
