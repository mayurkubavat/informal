// SEP-1865 MCP App host bridge for Antigravity (`ui://informal/workbench.html`).
// Uses `@modelcontextprotocol/ext-apps` when running inside an MCP App iframe
// (or `?mcpApp=1` during E2E tests) to synchronize host theme/CSS variables,
// call `informal-mcp` tools directly over postMessage, attach `@context` pills
// via `updateModelContext`, and open RTL files in Antigravity's editor pane.

import { App, applyDocumentTheme, applyHostStyleVariables } from "@modelcontextprotocol/ext-apps";

export const isMcpAppSurface: boolean = (() => {
  if (typeof window === "undefined") return false;
  if ("__TAURI_INTERNALS__" in window) return false;
  const q = new URLSearchParams(window.location.search);
  if (q.get("mcpApp") === "1") return true;
  if ((window as unknown as { __INFORMAL_MCP_APP__?: boolean }).__INFORMAL_MCP_APP__) return true;
  return false;
})();

class McpAppBridge {
  private app: App | null = null;
  private connected = false;
  private connecting: Promise<App | null> | null = null;
  private toolResultListeners = new Set<(structured: Record<string, unknown>) => void>();

  async ensureConnected(): Promise<App | null> {
    if (!isMcpAppSurface) return null;
    if (this.connected && this.app) return this.app;
    if (this.connecting) return this.connecting;

    this.connecting = (async () => {
      try {
        const app = new App({ name: "InFormal", version: "0.2.0" }, {});
        app.ontoolresult = (result) => {
          const sc = (result as { structuredContent?: Record<string, unknown> }).structuredContent;
          if (sc) {
            for (const cb of this.toolResultListeners) cb(sc);
          }
        };
        app.onhostcontextchanged = (ctx) => {
          if (ctx.theme) {
            applyDocumentTheme(ctx.theme);
            document.documentElement.dataset.theme = ctx.theme;
          }
          if (ctx.styles?.variables) {
            applyHostStyleVariables(ctx.styles.variables);
          }
        };
        await app.connect();
        this.app = app;
        this.connected = true;
        const ctx = app.getHostContext();
        if (ctx?.theme) {
          applyDocumentTheme(ctx.theme);
          document.documentElement.dataset.theme = ctx.theme;
        }
        if (ctx?.styles?.variables) {
          applyHostStyleVariables(ctx.styles.variables);
        }
        return app;
      } catch {
        return null;
      } finally {
        this.connecting = null;
      }
    })();

    return this.connecting;
  }

  onToolResult(cb: (structured: Record<string, unknown>) => void): () => void {
    this.toolResultListeners.add(cb);
    void this.ensureConnected();
    return () => {
      this.toolResultListeners.delete(cb);
    };
  }

  async callTool<T = Record<string, unknown>>(name: string, args: Record<string, unknown> = {}): Promise<T> {
    const app = await this.ensureConnected();
    if (!app) throw new Error("MCP App host is not connected");
    const res = await app.callServerTool({ name, arguments: args });
    const sc = (res as { structuredContent?: unknown; isError?: boolean; content?: { type?: string; text?: string }[] });
    if (sc.isError) {
      const msg = sc.content?.find((c) => c.type === "text")?.text ?? `${name} failed`;
      throw new Error(msg);
    }
    if (sc.structuredContent !== undefined) return sc.structuredContent as T;
    const raw = sc.content?.find((c) => c.type === "text")?.text;
    return (raw ? JSON.parse(raw) : {}) as T;
  }

  async attachContext(summaryText: string): Promise<boolean> {
    const app = await this.ensureConnected();
    if (!app) return false;
    const caps = app.getHostCapabilities();
    if (!caps?.updateModelContext) return false;
    try {
      await app.updateModelContext({
        content: summaryText ? [{ type: "text", text: summaryText }] : [],
      });
      return true;
    } catch {
      return false;
    }
  }

  async openFileInHostEditor(absPath: string): Promise<boolean> {
    const app = await this.ensureConnected();
    if (!app) return false;
    const uri = absPath.startsWith("file://") ? absPath : `file://${absPath.startsWith("/") ? "" : "/"}${absPath}`;
    try {
      const r = await (app as unknown as {
        request: (req: { method: string; params: Record<string, unknown> }, schema: unknown) => Promise<{ isError?: boolean }>;
      }).request(
        {
          method: "com.google.antigravity/toggle-aux-pane",
          params: { tabType: "fileView", uri },
        },
        { parse: (v: unknown) => v, safeParse: (v: unknown) => ({ success: true, data: v }) },
      );
      return !r?.isError;
    } catch {
      return false;
    }
  }
}

export const mcpBridge = new McpAppBridge();
