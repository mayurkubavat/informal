// Ask mode helpers: reading the model's answer and turning a suggested
// command into the prompt's pre-filled text. The backend (ui/src-tauri/
// src/ask.rs split_answer) does the same split; this copy serves the
// browser mock and keeps the console's display rules testable.
//
// Safety rule: nothing here runs anything. A suggested command is only put
// on the prompt; the user presses Enter to run it or Esc to discard it.

export type InputMode = "tcl" | "ask";

export interface LlmStatus {
  provider: string;
  model: string;
  ok: boolean;
  error?: string;
}

export interface LlmProviderEntry {
  name: string;
  kind: string;
  model: string;
  base_url: string;
  selected: boolean;
}

// Tauri events "ask-delta" and "ask-done" (docs/ui/ask-mode.md).
export interface AskDelta {
  id: number;
  text: string;
}
export interface AskDone {
  id: number;
  command?: string | null;
  explanation: string;
  error?: string | null;
  model?: string;
  first_token_ms?: number | null;
  total_ms?: number;
}

// The answer split into its explanation and the first ```tcl (or untagged)
// fenced block's contents. Blocks in other languages are skipped; an
// unclosed fence (answer cut short) still yields its text.
export function splitAnswer(text: string): { explanation: string; command: string | null } {
  let from = 0;
  for (;;) {
    const at = text.indexOf("```", from);
    if (at < 0) return { explanation: text.trim(), command: null };
    const nl = text.indexOf("\n", at);
    const lineEnd = nl < 0 ? text.length : nl;
    const tag = text.slice(at + 3, lineEnd).trim().toLowerCase();
    if (tag === "" || tag === "tcl") {
      const body = text.slice(Math.min(lineEnd + 1, text.length));
      const end = body.indexOf("```");
      const cmd = (end < 0 ? body : body.slice(0, end)).trim();
      const after = end < 0 ? "" : body.slice(end + 3).trim();
      const explanation = `${text.slice(0, at).trim()} ${after}`.trim();
      return { explanation, command: cmd || null };
    }
    const close = text.indexOf("```", lineEnd);
    if (close < 0) return { explanation: text.trim(), command: null };
    from = close + 3;
  }
}

// The prompt holds one line: a multi-line suggestion becomes one Tcl line
// joined with "; " (comment lines dropped).
export function prefill(command: string): string {
  return command
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"))
    .join("; ");
}

// How the console shows an answer while it streams: every line prefixed
// "ask: ", the fenced block's fence lines left out (its command is shown
// on the prompt instead once the answer is complete).
export function askLines(text: string): string[] {
  const lines = text.replace(/\s+$/, "").split("\n");
  return lines.filter((l) => !/^\s*```/.test(l)).map((l) => `ask: ${l}`);
}

// The status bar label.
export function llmLabel(s: LlmStatus | null): string {
  if (!s) return "LLM: …";
  if (!s.ok) return "LLM: offline";
  return `LLM: ${s.provider}/${s.model}`;
}

// Tooltip of the Ask switch and the status bar item.
export function llmTip(s: LlmStatus | null): string {
  if (!s) return "Checking the LLM provider…";
  if (!s.ok) return `Ask is unavailable: ${s.error ?? "the LLM provider does not answer"}`;
  return `Ask mode uses ${s.provider} (${s.model}). Click to pick another provider.`;
}
