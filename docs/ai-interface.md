# The AI interface

`informal mcp` serves one FPV session to an AI model as
[Model Context Protocol](https://modelcontextprotocol.io) tools. The session
is the one the shell and the desktop app use; the tools read it as data and
drive it within the limits of decision D17 (`docs/decisions.md`): an AI can
read anything and propose anything, but only a checked engine result is a
verdict.

Code: crate `informal-mcp` (`api.rs` the tools, `tools.rs` their schemas,
`server.rs` the protocol). Tests: `crates/informal-mcp/tests/session.rs`
(golden replies in `tests/golden/`, every reply checked against its schema,
a scripted client that finds a failing assertion and proposes a fix) and
`crates/informal-cli/tests/mcp.rs` (the process on standard input and
output, and `--connect`). In the app: `ui/src-tauri/src/ai.rs` (the socket),
`ui/src/components/Proposals.tsx` (the cards), `ui/tests/e2e/proposals.spec.ts`,
`ui/tests/e2e/window-tools.spec.ts`.

## Start it

```sh
informal mcp [--mode read-only|propose|auto] [--root DIR]
             [--engines config/engines.toml] [--workdir informal_work]
             [--max-time 300]
```

The server reads JSON-RPC 2.0 messages, one per line, on standard input and
answers on standard output (the protocol's stdio transport; revisions
2025-06-18, 2025-03-26 and 2024-11-05). Whatever the session prints (report
tables, a script's `puts`, engine output) goes to standard error. A client
configuration looks like this:

```json
{
  "mcpServers": {
    "informal": {
      "command": "informal",
      "args": ["mcp", "--mode", "propose", "--root", "/path/to/project"]
    }
  }
}
```

- `--root`: scripts and design files are only opened below this directory
  (default: where the server starts). Files of the open design and the
  scripts that created its properties can be read wherever they are.
- `--max-time`: the longest time limit, in seconds, one `prove` call may ask
  for.
- Without the engines file, the in-house engines serve alone.

## Modes

| Mode | Read, prove | Add an assumption, assertion, cover, cut point | Tcl command |
| --- | --- | --- | --- |
| `read-only` | yes | no (the tools are not offered) | no |
| `propose` (default) | yes | recorded as a proposal; waits for a person | recorded as a proposal; waits for a person |
| `auto` | yes | applied at once, tagged AI-added | runs |

`auto` is meant for an evaluation sandbox. In `propose` mode the person's
answer comes from the front end they work in (`Api::decide`): the desktop
app shows each proposal as a card with Accept and Reject (below). A server
started on its own with `informal mcp` has no such front end, so a proposal
made there stays pending.

## The session a person is looking at

The desktop app serves its own session, so an agent and a person work on
the same design, properties and results:

```sh
informal mcp --connect            # the most recently started app
informal mcp --connect SOCKET     # a particular app's socket
```

`--connect` starts no session; it passes the protocol between its standard
streams and the app's socket, so a client is configured as above with
`"args": ["mcp", "--connect"]`.

- The app listens on a Unix socket in a directory only the user can enter
  (`$XDG_RUNTIME_DIR/informal/app-<pid>.sock`). The environment variable
  `INFORMAL_AI_SOCKET` moves it, or turns it off with `off`;
  `INFORMAL_AI_MODE` sets the mode (default `propose`).
- What the agent proposes appears in the app's **Proposals** tab as a card:
  what it is, its text, the agent's reason, and Accept and Reject. The tab
  and a note in the status bar appear when there is a proposal; nothing
  else moves. Accepting applies the change, tagged **AI** in the property
  table; a result found with an AI-added change in the model is marked
  **under AI**, and the Details tab names what it stands on. Both answers
  are recorded in the console and in Messages.
- The agent can see and drive the window (the window tools below): what
  the person selected, the open trace and where their cursor is, and it can
  select a property, open a trace at a cycle, add signals to the waveform
  and highlight source lines. Each such action is noted in Messages, the
  console keeps the dock, and the person's next selection takes the source
  view back.
- After each of the agent's calls that may have changed something, the app
  redraws from the session. A prove the agent starts shows its progress
  like any other, and Stop stops it.
- While a command of the person's runs or waits, the agent's tools answer
  `busy` at once. A client that hangs up stops only a prove it started.

## Tools

Every reply is one JSON object with a `schema` field that names its shape
and version, such as `informal.trace/1`. The protocol carries it as
`structuredContent`, and as the same JSON in a text block for clients that
only read text. Each tool publishes JSON Schemas for its arguments and its
reply (`tools/list`).

| Group | Tool | What it gives | Changes the session |
| --- | --- | --- | --- |
| Session | `session_open` | Start over from a Tcl script, or from files with top, clock and reset; the summary | starts it over |
| | `script_check` | What a script uses that the tool rejects or ignores, per line, without running it | no |
| | `session_summary` | Setup, engines, property counts by status, what an AI added or proposed | no |
| | `session_log` | The last console lines, including every AI-added change | no |
| Properties | `property_list` | Properties with status, filtered by kind, status, name, origin or task | no |
| | `property_get` | One property: status, engine, time, depth, source location, task, what the last prove measured (`stats`), vacuity, every engine's run | no |
| | `property_cone` | Inputs and registers in the property's cone of influence, by register distance | no |
| Design | `design_hierarchy` | Instance tree with ports and signals | no |
| | `design_info` | Counts of the design (inputs, flops, latches, memories, clocks and resets found) and the size of the current task's model (Btor2 nodes, states, AIG ands) | no |
| | `design_source` | Source text by file and line range | no |
| Traces | `trace_get` | A counterexample or cover trace: values per signal and frame, chosen signals, a frame window | no |
| Prove | `prove` | Run the engines on named properties, or on one task's, with a time limit; each one's result | statuses only |
| | `prove_status` | Progress of a running prove; the result of one started with `wait: false` | no |
| | `prove_stop` | Stop a running prove | no |
| Proposals | `proposal_add` | Propose an assumption, helper assertion, cover or cut point | yes, by mode |
| | `proposal_list` | Every proposal and its state | no |
| | `proposal_undo` | Withdraw or remove proposals, one or all; results that stood on them are reset | yes |
| Escape hatch | `tcl_eval` | Evaluate a Tcl command in the session's shell | yes, by mode |

### Window tools

Offered only on a desktop app's session, in every mode: they change the
window, never the session.

| Tool | What it does |
| --- | --- |
| `gui_state` | What the person is looking at: the selected property, the view (table, source or trace), the table's filter, the open trace with the cursor's frame and the signals in the waveform, the source file and highlighted lines |
| `gui_select_property` | Select a property in the table |
| `gui_open_trace` | Open a cex or covered property's waveform, with the cursor on a frame |
| `gui_add_signals` | Add signals, by full name, to the open waveform |
| `gui_highlight_source` | Open a file in the source view and highlight a range of lines |

Each returns the window's state after it (`informal.gui/1`), or the
window's reason when it cannot be done ("a_max has no trace: its status is
proven"). In a design on both clock edges or with several clocks the
window counts clock cycles where `trace_get` counts model steps, so a
frame number does not carry over there.

A tool that fails answers with `isError` and
`{"schema": "informal.error/1", "code": ..., "message": ...}`. Codes:
`arguments`, `not_found`, `busy`, `mode`, `rejected` (the session or the
window refused it, with its reason), `no_trace`, `path`, `no_gui`,
`internal`.

### A property in a reply

```json
{
  "name": "a_wrong", "kind": "assert", "expr": "count != 3'd3",
  "enabled": true, "origin": "user",
  "status": "cex", "engine": "informal-bmc", "time_ms": 42, "depth": 4,
  "has_trace": true,
  "source": {"file": "/path/to/fifo.jg.tcl", "line": 15}
}
```

- `origin`: `user` (a script or the console), `rtl` (written in the design)
  or `ai` (added through this interface).
- `status`: `unprocessed`, `proven`, `cex`, `covered`, `unreachable`,
  `undetermined` or `error`; `detail` says why for the last two.
- `under_ai`: the AI-added assumptions (by name) and cut points
  (`stopat <signal>`) that were in the model when the status was found. The
  result stands on them.

### A trace as data

```json
{
  "schema": "informal.trace/1",
  "unit": "cycle", "frames": 5, "first": 0, "last": 4, "hit_at": [4],
  "signals": [
    {"name": "fifo.count", "width": 3, "values": ["7", "0", "1", "2", "3"]},
    {"name": "fifo.push",  "width": 1, "values": ["1", "1", "1", "1", "1"]}
  ],
  "signals_omitted": 0
}
```

A frame is a clock cycle, and frame 0 is the reset cycle, where registers
still show their free value. For a design on both clock edges or with
several clocks, `unit` is `model_step`: half a period of the fastest clock.
Words are in hex, single bits `0` or `1`, and a digit nothing in the trace
gives is `x`. `hit_at` lists the frames in which the assertion fails or the
cover is hit. Without `signals`, the signals the property names come first,
then the rest of its cone, then the others; at most 64 signals and 256
frames come in one reply.

## What an AI-added change means

- It is tagged: `origin: "ai"` in every listing, `[AI-added]` in the report
  table, and a line in the log.
- A proof found with an AI-added assumption in the model is a proof under
  that assumption, and says so (`under_ai`; `[under AI-added: ...]` in the
  table). An AI-added cut point frees a signal: a proof still holds for the
  design, a counterexample may not.
- `proposal_undo` removes it in one step and sets every result that stood
  on it back to unprocessed.
- A Tcl command that ran (`tcl_eval` in `auto` mode) is the exception: it is
  logged as a proposal but cannot be undone.

## Limits today

- One request at a time: while a prove, a script or a Tcl command runs,
  other tools answer `busy`; `prove_status`, `prove_stop`, `session_log` and
  the protocol's cancellation and progress notifications work meanwhile.
- No check yet that an AI-added assumption over-constrains the design or
  makes a proof vacuous (planned).
- The app's own Ask mode uses the tools that read (`docs/ui/ask-mode.md`);
  it does not prove or propose.
