// Layer 5 — Real-Backend HTTP Bridge for Playwright E2E tests.
// Spawns the REAL compiled Rust `informal mcp --mode auto` binary (zero mock.ts!)
// and exposes the exact Tauri IPC RPC surface (`snapshot`, `shell_eval`,
// `shell_source`, `shell_flavor`, `read_text`, `read_trace`, `workbench_tool`)
// over a loopback HTTP server so Playwright drives real `svlang` + real solvers!

import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(__dirname, "../../..");

export function findInformalBinary() {
  const candidates = [
    process.env.INFORMAL_BIN,
    path.join(REPO_ROOT, "target/release/informal"),
    path.join(REPO_ROOT, "target/debug/informal"),
  ].filter(Boolean);
  for (const c of candidates) {
    if (fs.existsSync(c)) return c;
  }
  return null;
}

class McpClient {
  constructor(bin, workdir) {
    this.proc = spawn(
      bin,
      [
        "mcp",
        "--mode",
        "auto",
        "--root",
        REPO_ROOT,
        "--workdir",
        workdir,
        "--engines",
        path.join(REPO_ROOT, "config/engines.toml"),
      ],
      { cwd: REPO_ROOT, stdio: ["pipe", "pipe", "pipe"] },
    );
    this.nextId = 1;
    this.pending = new Map();
    this.buf = "";
    this.stderr = "";
    this.proc.stderr.on("data", (d) => {
      this.stderr += d.toString("utf8");
    });
    this.proc.stdout.on("data", (chunk) => {
      this.buf += chunk.toString("utf8");
      let nl;
      while ((nl = this.buf.indexOf("\n")) >= 0) {
        const line = this.buf.slice(0, nl).trim();
        this.buf = this.buf.slice(nl + 1);
        if (!line) continue;
        let msg;
        try {
          msg = JSON.parse(line);
        } catch {
          continue;
        }
        if (msg.id != null && this.pending.has(msg.id)) {
          const { resolve, reject } = this.pending.get(msg.id);
          this.pending.delete(msg.id);
          if (msg.error) reject(new Error(msg.error.message || JSON.stringify(msg.error)));
          else resolve(msg.result);
        }
      }
    });
    this.proc.on("exit", (code) => {
      for (const { reject } of this.pending.values()) {
        reject(new Error(`informal mcp exited with code ${code}: ${this.stderr.slice(-400)}`));
      }
      this.pending.clear();
    });
  }

  request(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  async init() {
    await this.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "informal-e2e-bridge", version: "0.1" },
    });
    this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  }

  async callTool(name, args = {}) {
    const res = await this.request("tools/call", { name, arguments: args });
    if (res.structuredContent) return res.structuredContent;
    const text = res.content?.find((c) => c.type === "text")?.text ?? "{}";
    return JSON.parse(text);
  }

  async evalTcl(command) {
    return this.callTool("tcl_eval", { command, rationale: "e2e bridge" });
  }

  async session() {
    const r = await this.evalTcl("informal::session");
    if (!r.ok) throw new Error(r.error || "informal::session failed");
    return JSON.parse(r.result);
  }

  close() {
    try {
      this.proc.stdin.end();
      this.proc.kill("SIGTERM");
    } catch {
      // ignore
    }
  }
}

function writeAxiTemplate(dir) {
  const sv = path.join(dir, "axi_lite_demo.sv");
  fs.writeFileSync(
    sv,
    `module axi_lite_demo (
  input  logic        clk,
  input  logic        rst_n,
  output logic        awvalid,
  input  logic        awready,
  output logic [31:0] awaddr,
  output logic        wvalid,
  input  logic        wready,
  output logic [31:0] wdata,
  output logic [3:0]  wstrb,
  input  logic        bvalid,
  output logic        bready,
  input  logic [1:0]  bresp,
  output logic        arvalid,
  input  logic        arready,
  output logic [31:0] araddr,
  input  logic        rvalid,
  output logic        rready,
  input  logic [31:0] rdata,
  input  logic [1:0]  rresp
);
  assign bready = 1'b1;
  assign rready = 1'b1;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      awvalid <= 1'b0;
      awaddr  <= 32'h0;
      wvalid  <= 1'b0;
      wdata   <= 32'h0;
      wstrb   <= 4'hf;
      arvalid <= 1'b0;
      araddr  <= 32'h0;
    end else begin
      if (!awvalid) begin
        awvalid <= 1'b1;
        awaddr  <= awaddr + 32'h4;
      end else if (awready) begin
        awvalid <= 1'b0;
      end
      if (!wvalid) begin
        wvalid <= 1'b1;
        wdata  <= wdata + 32'h1;
        wstrb  <= 4'hf;
      end else if (wready) begin
        wvalid <= 1'b0;
      end
      if (!arvalid) begin
        arvalid <= 1'b1;
        araddr  <= araddr + 32'h4;
      end else if (arready) begin
        arvalid <= 1'b0;
      end
    end
  end
endmodule
`,
  );
  return sv;
}

export async function startRealBackendBridge() {
  const bin = findInformalBinary();
  if (!bin) throw new Error("No compiled informal binary found in target/{release,debug}/informal");
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "informal-real-e2e-"));
  const tplDir = path.join(workdir, "templates");
  fs.mkdirSync(tplDir, { recursive: true });
  const mcp = new McpClient(bin, workdir);
  await mcp.init();
  let flavor = "jg";

  // Serialize RPCs against single-threaded Tcl session
  let queue = Promise.resolve();
  const serial = (fn) => {
    const next = queue.then(fn, fn);
    queue = next.catch(() => {});
    return next;
  };

  async function handleRpc(cmd, args = {}) {
    if (cmd === "snapshot") {
      return { ok: true, result: "", session: await mcp.session() };
    }
    if (cmd === "current_flavor") return flavor;
    if (cmd === "shell_flavor") {
      flavor = String(args.flavor || "jg");
      return { ok: true, result: flavor, session: await mcp.session() };
    }
    if (cmd === "shell_eval") {
      const r = await mcp.evalTcl(String(args.script || ""));
      return { ok: Boolean(r.ok), result: r.ok ? String(r.result ?? "") : String(r.error ?? ""), session: await mcp.session() };
    }
    if (cmd === "shell_source") {
      const p = String(args.path || "");
      let script;
      if (p === "template:axi4lite") {
        const sv = writeAxiTemplate(tplDir);
        // Deliberately leave `<embedded>` current (0 props) in one mode (`axi4lite-unfocused`)
        // or set `Protocol` current (`axi4lite` matching Tauri).
        script = `clear -all; analyze -sv12 {${sv}}; elaborate -top axi_lite_demo; clock clk; reset -expression {!rst_n}; check_protocol -create -protocol axi4lite -role master; task -set Protocol`;
      } else if (p === "template:axi4lite-empty-embedded") {
        const sv = writeAxiTemplate(tplDir);
        script = `clear -all; analyze -sv12 {${sv}}; elaborate -top axi_lite_demo; clock clk; reset -expression {!rst_n}; check_protocol -create -protocol axi4lite -role master`;
      } else {
        const abs = path.isAbsolute(p) ? p : path.resolve(REPO_ROOT, p);
        script = `clear -all; source {${abs}}`;
      }
      const r = await mcp.evalTcl(script);
      return { ok: Boolean(r.ok), result: r.ok ? String(r.result ?? "") : String(r.error ?? ""), session: await mcp.session() };
    }
    if (cmd === "shell_stop") {
      return { ok: true, result: "", session: await mcp.session() };
    }
    if (cmd === "read_text" || cmd === "read_trace") {
      const p = String(args.path || "");
      const abs = path.isAbsolute(p) ? p : path.resolve(REPO_ROOT, p);
      return fs.existsSync(abs) ? fs.readFileSync(abs, "utf8") : "";
    }
    if (cmd === "workbench_tool") {
      return mcp.callTool(String(args.tool), args.args || {});
    }
    if (cmd === "ai_status") return { mode: "propose", socket: null, proposals: [] };
    if (cmd === "llm_status") return { provider: "none", model: "", ok: false, error: "disabled in e2e" };
    if (cmd === "llm_providers") return [];
    if (cmd === "ui_state") return null;
    if (cmd === "surfer_available") return false;
    if (cmd === "shell_complete") return [];
    return null;
  }

  const server = http.createServer((req, res) => {
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Methods", "POST, OPTIONS");
    res.setHeader("Access-Control-Allow-Headers", "content-type");
    if (req.method === "OPTIONS") {
      res.writeHead(204);
      res.end();
      return;
    }
    if (req.method === "POST" && req.url === "/rpc") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", async () => {
        try {
          const { cmd, args } = JSON.parse(body || "{}");
          const out = await serial(() => handleRpc(cmd, args));
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify(out));
        } catch (err) {
          res.writeHead(500, { "content-type": "text/plain" });
          res.end(String(err));
        }
      });
      return;
    }
    res.writeHead(404);
    res.end("not found");
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return {
    url: `http://127.0.0.1:${port}`,
    async stop() {
      mcp.close();
      await new Promise((resolve) => server.close(resolve));
      fs.rmSync(workdir, { recursive: true, force: true });
    },
  };
}
