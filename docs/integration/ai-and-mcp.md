# AI & MCP Integration

InFormal provides a **verifier-gated Model Context Protocol (MCP)** interface (`informal mcp`) so AI coding agents can operate the formal engine like an expert verification engineer while preserving mathematical soundness: **an AI can inspect anything and propose anything, but only a checked formal engine result is a verdict** (decision D17).

---

## 1. Connecting Your AI Client

### Option A — Antigravity Plugin (Bundled)

This repository ships a ready-to-install Antigravity plugin under [`plugin/`](../../plugin/README.md) containing:
- `mcp_config.json` and `bin/informal-mcp-launcher.sh` (auto-connects to a running `InFormal.app` socket, or falls back to a standalone stdio server with `--root "${INFORMAL_ROOT:-$HOME}"`)
- Progressive-disclosure expert skills in [`plugin/skills/informal-fpv/SKILL.md`](../../plugin/skills/informal-fpv/SKILL.md) plus deep playbooks (`cex-triage-playbook.md`, `convergence-playbook.md`, `spec-to-sva-guide.md`)
- Embedded interactive MCP App workbench (`open_workbench`)

### Option B — Generic MCP Client (Claude Code, Cursor, Codex, Custom)

**Headless standalone session** (starts its own FPV session over stdio JSON-RPC 2.0):

```json
{
  "mcpServers": {
    "informal": {
      "command": "informal",
      "args": ["mcp", "--mode", "propose", "--root", "/path/to/your/rtl/project"]
    }
  }
}
```

**Live co-working with `InFormal.app`** (shares the exact session you are looking at in the desktop app):

```json
{
  "mcpServers": {
    "informal": {
      "command": "informal",
      "args": ["mcp", "--connect"]
    }
  }
}
```

Pass an explicit socket path (`["mcp", "--connect", "/path/to/app.sock"]`) if multiple `InFormal.app` windows are open; without a path, `--connect` attaches to the newest running app socket.

---

## 2. Permission Modes & Security Boundaries

| Mode | Inspect & Prove | `proposal_validate` | Propose / Apply Edits & `tcl_eval` |
| --- | --- | --- | --- |
| `read-only` | Yes | Yes (zero side effects) | Disabled (tools omitted from `tools/list`) |
| `propose` *(default)* | Yes | Yes | Recorded as pending Proposals awaiting human Accept/Reject |
| `auto` *(eval sandboxes)* | Yes | Yes | Applied immediately, tagged **AI** (dependent proofs marked **`under_ai`**) |

Additional guardrails:
- `--root <DIR>`: `session_open` and `formal_fix_apply` reject paths outside `<DIR>`.
- `--max-time <SECS>`: caps any single `prove` call requested by an agent (default `300`).

---

## 3. Recommended Expert Agent Workflow (CEGIS Loop)

InFormal exposes high-level composite workflow tools and server-hosted **MCP Prompts** so agents solve formal tasks in 2–3 tool calls instead of 15+ noisy round trips:

1. **Diagnose in one call:**
   - Failing assertion (`cex`) → call **`cex_diagnose`** (fuses `property_get`, `why_cex`, trimmed trace, primary-input vs. DUT-state roles, driver source snippets, and `formal_lint`).
   - Timed-out property (`undetermined`) → call **`convergence_diagnose`** (fuses engine telemetry, COI counts, `fv_complexity` hotspots, active constraints, and ranked tactics).
2. **Validate before proposing (zero side effects):**
   - Call **`proposal_validate`** on any candidate assumption, helper assertion, cover, or cut point. It clones the active task into a scratch task and verifies syntax, `SV-FPV-001` primary-input hygiene, assumption consistency + conflict core, 2QBF dead-end freedom, cover non-regression, vacuity, and target impact — then deletes the scratch task cleanly.
3. **Propose & present visually:**
   - Only after `proposal_validate` reports sound, call **`proposal_add`** (or **`formal_fix_apply`** for RTL quick-fixes).
   - When attached to `InFormal.app`, call **`gui_present_cex`** to switch the user's window to `CEX Debug`, pin causal waveform callouts, and launch an interactive walkthrough.
4. **Gate signoff mechanically:**
   - Call **`spec_audit`** against `fvp.spec.json` (`informal.spec/1`) to audit every `REQ-*` item, assumption boundary, cover reachability, and signoff threshold.

---

## 4. Server-Hosted MCP Prompts

Query `prompts/list` or invoke `prompts/get` in any MCP client that supports prompts — each playbook is automatically hydrated with the live session's top module, primary inputs/outputs, clocks/reset, and failing/undetermined properties:

| Prompt | Use case |
| --- | --- |
| `triage_cex` | Root-cause a failing assertion (`cex_diagnose` → `proposal_validate` → `proposal_add` / `gui_present_cex`) |
| `close_undetermined` | Converge a timed-out proof (`convergence_diagnose` → helper invariants / `stopat` / abstraction) |
| `spec_to_sva` | Formalize a hardware requirement into reset-guarded SVA + witness cover respecting DUT port boundary |
| `audit_signoff` | Full signoff audit (`spec_audit`, `formal_lint`, vacuity, `under_ai`, `check_cov`) |

---

## 5. AI-Readable Documentation (`llms.txt` & `index.json`)

To ground any external AI agent without bloating its context window, point it at:
- [`llms.txt`](../../llms.txt) — concise documentation map
- [`docs/reference/index.json`](../reference/index.json) — exact machine-readable CLI, Tcl, MCP tool, and prompt catalog

---

## Related References

- Auto-generated schemas for all 48 MCP tools: [MCP Tools & Prompts Reference](../reference/mcp-tools.md)
- `fvp.spec.json` schema: [Formal Spec Schema](../reference/spec-schema.md)
- Implementation architecture & protocol details: [docs/ai-interface.md](../ai-interface.md)
