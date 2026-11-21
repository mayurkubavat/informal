// Standalone desktop Launchpad (`WelcomePane`): shown when InFormal starts
// with no elaborated design or properties (`?welcome=1` in browser preview).
// Supports drag-and-drop `.tcl` / `.sv` / `.f` files, recent sessions, 1-click
// starter templates, and 1-click MCP AI Agent socket copy.

import { useState } from "react";
import type { AiStatus } from "../backend";
import type { RecentSession } from "../store/workbenchStore";
import { Logo } from "./Logo";

const STARTER_TEMPLATES = [
  {
    id: "counter-fpv",
    title: "Saturating Counter & Witness",
    script: "tests/fpv/counter/counter.jg.tcl",
    desc: "Assertions, reachability covers, CEX trace slicing, and RTL cone schematic.",
  },
  {
    id: "fifo-scoreboard",
    title: "Parameterized Sync FIFO",
    script: "tests/fpv/fifo/fifo.jg.tcl",
    desc: "Data integrity invariants, pointer wrap proofs, and AI assumption proposals.",
  },
  {
    id: "protocol-axi4lite",
    title: "AXI4-Lite Bus Protocol VIP",
    script: "template:axi4lite",
    desc: "1-command formal protocol VIP generation with handshake stability checks.",
  },
  {
    id: "csr-ral-signoff",
    title: "CSR Register Lock & Signoff",
    script: "template:csr",
    desc: "Automated RW/RO/W1C register verification and COI coverage signoff.",
  },
];

export function WelcomePane({
  ai,
  recent,
  onOpenScript,
  onOpenPalette,
}: {
  ai: AiStatus | null;
  recent: RecentSession[];
  onOpenScript: (path: string) => void;
  onOpenPalette: () => void;
}) {
  const [dragging, setDragging] = useState(false);
  const [customPath, setCustomPath] = useState("");
  const [copied, setCopied] = useState(false);

  const mcpCmd = ai?.socket ? `informal mcp --connect ${ai.socket}` : "informal mcp --connect";

  return (
    <div
      className={`welcome-pane ${dragging ? "drag-over" : ""}`}
      onDragOver={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragging(false);
        const file = e.dataTransfer.files?.[0];
        if (file) {
          const anyFile = file as File & { path?: string };
          onOpenScript(anyFile.path || file.name);
        }
      }}
    >
      <div className="welcome-card">
        <div className="welcome-hero">
          <Logo size={46} />
          <div>
            <h1 className="welcome-title">InFormal</h1>
            <p className="welcome-subtitle muted">
              Pure-Rust IEEE 1800 Formal Property Verification · Causal Slicing · AI Co-Pilot
            </p>
          </div>
          <span className="spacer" />
          <button className="btn" onClick={onOpenPalette} title="Open Command Palette (Cmd+K / Ctrl+K)">
            Command Palette <kbd className="palette-kbd mono">⌘K</kbd>
          </button>
        </div>

        <form
          className="welcome-open-row"
          onSubmit={(e) => {
            e.preventDefault();
            if (customPath.trim()) onOpenScript(customPath.trim());
          }}
        >
          <input
            className="welcome-input mono"
            placeholder="Open .tcl run script, .f filelist, or drop files anywhere…"
            aria-label="Open script or design path"
            value={customPath}
            onChange={(e) => setCustomPath(e.target.value)}
          />
          <button type="submit" className="btn welcome-primary-btn" disabled={!customPath.trim()}>
            Load Session
          </button>
        </form>

        <div className="welcome-grid">
          <section className="welcome-section" aria-label="Starter workspaces">
            <h2 className="welcome-h2">Quick-Start Workspaces</h2>
            <div className="welcome-templates">
              {STARTER_TEMPLATES.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className="welcome-tpl-btn"
                  onClick={() => onOpenScript(t.script)}
                >
                  <span className="strong">{t.title}</span>
                  <span className="small muted">{t.desc}</span>
                  <span className="mono small dim">{t.script}</span>
                </button>
              ))}
            </div>
          </section>

          <div className="welcome-side">
            {recent.length > 0 && (
              <section className="welcome-section" aria-label="Recent sessions">
                <h2 className="welcome-h2">Recent Sessions</h2>
                <div className="welcome-recent-list">
                  {recent.map((r) => (
                    <button
                      key={r.path}
                      type="button"
                      className="welcome-recent-item mono small ellipsis"
                      title={r.path}
                      onClick={() => onOpenScript(r.path)}
                    >
                      {r.path}
                    </button>
                  ))}
                </div>
              </section>
            )}

            <section className="welcome-section welcome-ai-box welcome-mcp" aria-label="AI Co-Pilot & MCP connection">
              <h2 className="welcome-h2">✦ AI Co-Pilot & Antigravity MCP</h2>
              <p className="small muted">
                Connect Antigravity, Claude Code, or Cursor to drive proofs, inspect <code>why_cex</code> slices, and render live waveforms via <code>ui://informal/workbench.html</code>.
              </p>
              <div className="welcome-mcp-row">
                <code className="welcome-code" tabIndex={0} aria-label="MCP connect command" title={mcpCmd}>{mcpCmd}</code>
                <button
                  type="button"
                  className="btn small"
                  onClick={() => {
                    navigator.clipboard?.writeText(mcpCmd).then(() => {
                      setCopied(true);
                      setTimeout(() => setCopied(false), 1800);
                    }).catch(() => {});
                  }}
                >
                  {copied ? "Copied" : "Copy"}
                </button>
              </div>
            </section>
          </div>
        </div>
      </div>
    </div>
  );
}
