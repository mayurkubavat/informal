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
  if (process.env.INFORMAL_BIN && fs.existsSync(process.env.INFORMAL_BIN)) {
    return process.env.INFORMAL_BIN;
  }
  const candidates = [
    path.join(REPO_ROOT, "target/release/informal"),
    path.join(REPO_ROOT, "target/debug/informal"),
  ].filter((c) => fs.existsSync(c));
  if (candidates.length === 0) return null;
  candidates.sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs);
  return candidates[0];
}

class McpClient {
  constructor(bin, workdir, rootDir = REPO_ROOT) {
    this.proc = spawn(
      bin,
      [
        "mcp",
        "--mode",
        "auto",
        "--root",
        rootDir,
        "--workdir",
        workdir,
        "--engines",
        path.join(REPO_ROOT, "config/engines.toml"),
      ],
      { cwd: rootDir, stdio: ["pipe", "pipe", "pipe"] },
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

function parseVcdMeta(vcd) {
  const scope = [];
  const signals = [];
  let frames = null;
  let steps = false;
  let maxTime = 0;
  for (const raw of vcd.split(/\r?\n/)) {
    const l = raw.trim();
    if (!l) continue;
    const w = l.split(/\s+/);
    if (w[0] === "$comment") {
      const body = w.slice(1, -1).join(" ");
      if (body.startsWith("informal trace of ")) {
        const count = body.split(": ").pop() || "";
        const n = Number.parseInt(count.split(" ")[0], 10);
        if (Number.isFinite(n)) frames = n;
        steps = count.endsWith("model steps");
      }
    } else if (w[0] === "$scope" && w[2]) {
      scope.push(w[2]);
    } else if (w[0] === "$upscope") {
      scope.pop();
    } else if (w[0] === "$var" && w.length >= 6) {
      const width = Number.parseInt(w[2], 10) || 1;
      const leaf = w[4];
      signals.push({
        name: leaf,
        path: [...scope, leaf].join("."),
        width,
        clock: signals.length === 0,
      });
    } else if (l.startsWith("#")) {
      const t = Number.parseInt(l.slice(1), 10);
      if (Number.isFinite(t) && t > maxTime) maxTime = t;
    }
  }
  return { frames: frames ?? Math.max(1, Math.floor(maxTime / 10)), steps, signals };
}

function encodeVcdToIftrBuffer(vcd, firstFrame, lastFrame, sigFilter) {
  const scope = [];
  const sigs = [];
  const byId = new Map();
  let framesComment = null;
  let steps = false;
  let inDefs = true;
  let frame = null;
  let maxFrame = 0;

  for (const raw of vcd.split(/\r?\n/)) {
    const l = raw.trim();
    if (!l) continue;
    if (inDefs) {
      const w = l.split(/\s+/);
      if (w[0] === "$comment") {
        const body = w.slice(1, -1).join(" ");
        if (body.startsWith("informal trace of ")) {
          const count = body.split(": ").pop() || "";
          const n = Number.parseInt(count.split(" ")[0], 10);
          if (Number.isFinite(n)) framesComment = n;
          steps = count.endsWith("model steps");
        }
      } else if (w[0] === "$scope" && w[2]) {
        scope.push(w[2]);
      } else if (w[0] === "$upscope") {
        scope.pop();
      } else if (w[0] === "$var" && w.length >= 6) {
        const width = Number.parseInt(w[2], 10) || 1;
        byId.set(w[3], sigs.length);
        sigs.push({
          path: [...scope, w[4]].join("."),
          width,
          clock: sigs.length === 0,
          runs: [],
        });
      } else if (w[0] === "$enddefinitions") {
        inDefs = false;
      }
      continue;
    }
    if (l.startsWith("#")) {
      const t = Number.parseInt(l.slice(1), 10);
      frame = t % 10 === 0 ? Math.floor(t / 10) : null;
      continue;
    }
    if (frame === null) continue;
    let bits;
    let id;
    if (l.startsWith("b")) {
      const sp = l.indexOf(" ");
      if (sp < 0) continue;
      bits = l.slice(1, sp);
      id = l.slice(sp + 1).trim();
    } else if ("01xz".includes(l[0])) {
      bits = l[0];
      id = l.slice(1);
    } else {
      continue;
    }
    const idx = byId.get(id);
    if (idx === undefined) continue;
    const s = sigs[idx];
    const padded = bits.padStart(s.width, "0");
    const last = s.runs[s.runs.length - 1];
    if (last && last[0] === frame) last[1] = padded;
    else if (!last || last[1] !== padded) s.runs.push([frame, padded]);
    if (frame > maxFrame) maxFrame = frame;
  }

  const totalFrames = Math.max(1, framesComment ?? maxFrame + 1);
  const clampedFirst = Math.max(0, Math.min(totalFrames - 1, firstFrame));
  const clampedLast = Math.max(clampedFirst, Math.min(totalFrames - 1, lastFrame));
  const selected = sigs.filter((s) => !sigFilter || s.clock || sigFilter.includes(s.path));

  const chunks = [];
  const pushU8 = (v) => chunks.push(Buffer.from([v & 0xff]));
  const pushU16 = (v) => {
    const b = Buffer.alloc(2);
    b.writeUInt16LE(v, 0);
    chunks.push(b);
  };
  const pushU32 = (v) => {
    const b = Buffer.alloc(4);
    b.writeUInt32LE(v >>> 0, 0);
    chunks.push(b);
  };

  chunks.push(Buffer.from("IFTR", "ascii"));
  pushU8(1);
  pushU8(steps ? 1 : 0);
  pushU32(totalFrames);
  pushU32(selected.length);

  for (const s of selected) {
    const nb = Buffer.from(s.path, "utf8");
    pushU16(nb.length);
    chunks.push(nb);
    pushU16(s.width);
    pushU8(s.clock ? 1 : 0);

    // Clip runs to [clampedFirst..=clampedLast] carrying initial state at clampedFirst.
    const clipped = [];
    let cur = "x".repeat(s.width);
    for (const [rf, val] of s.runs) {
      if (rf <= clampedFirst) {
        cur = val;
      } else if (rf <= clampedLast) {
        if (clipped.length === 0) clipped.push([clampedFirst, cur]);
        if (clipped[clipped.length - 1][1] !== val) clipped.push([rf, val]);
        cur = val;
      } else {
        break;
      }
    }
    if (clipped.length === 0) clipped.push([clampedFirst, cur]);

    pushU32(clipped.length);
    for (const [rf, val] of clipped) {
      pushU32(rf);
      const vb = Buffer.from(val, "ascii");
      pushU16(vb.length);
      chunks.push(vb);
    }
  }
  return Buffer.concat(chunks);
}

export async function startRealBackendBridge(opts = {}) {
  const bin = findInformalBinary();
  if (!bin) throw new Error("No compiled informal binary found in target/{release,debug}/informal");
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), "informal-real-e2e-"));
  const rootDir = opts.root || REPO_ROOT;
  const tplDir = path.join(workdir, "templates");
  fs.mkdirSync(tplDir, { recursive: true });
  const mcp = new McpClient(bin, workdir, rootDir);
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
      const ns = flavor === "vcf" ? "vcf" : "jg";
      await mcp.evalTcl(`namespace eval :: { namespace import -force ::${ns}::* }`);
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
      await mcp.callTool("prove_stop", {});
      const wb = await mcp.callTool("open_workbench", {});
      return { ok: true, result: "", session: wb.session || null };
    }
    if (cmd === "read_text" || cmd === "read_trace") {
      const p = String(args.path || "");
      const abs = path.isAbsolute(p) ? p : path.resolve(REPO_ROOT, p);
      return fs.existsSync(abs) ? fs.readFileSync(abs, "utf8") : "";
    }
    if (cmd === "read_trace_meta") {
      const p = String(args.path || "");
      const abs = path.isAbsolute(p) ? p : path.resolve(REPO_ROOT, p);
      if (!fs.existsSync(abs)) return null;
      const fstAbs = abs.replace(/\.vcd$/i, ".fst");
      const hasFst = fs.existsSync(fstAbs) && fs.statSync(fstAbs).size > 0;
      const vcd = fs.readFileSync(abs, "utf8");
      const { frames, steps, signals } = parseVcdMeta(vcd);
      return {
        frames,
        steps,
        format: hasFst ? "fst" : "vcd",
        block_count: Math.max(1, Math.ceil(frames / 256)),
        signals,
      };
    }
    if (cmd === "read_trace_bin" || cmd === "read_trace_window") {
      const p = String(args.path || "");
      const abs = path.isAbsolute(p) ? p : path.resolve(REPO_ROOT, p);
      if (!fs.existsSync(abs)) return null;
      const vcd = fs.readFileSync(abs, "utf8");
      const firstFrame = cmd === "read_trace_window" ? Number(args.firstFrame ?? 0) : 0;
      const lastFrame = cmd === "read_trace_window" ? Number(args.lastFrame ?? 1e9) : 1e9;
      const sigFilter = cmd === "read_trace_window" && Array.isArray(args.signals) ? args.signals : null;
      const buf = encodeVcdToIftrBuffer(vcd, firstFrame, lastFrame, sigFilter);
      return buf.toString("base64");
    }
    if (cmd === "workbench_tool") {
      return mcp.callTool(String(args.tool), args.args || {});
    }
    if (cmd === "ai_status") return { mode: "propose", socket: null, proposals: [] };
    if (cmd === "llm_status") return { provider: "none", model: "", ok: false, error: "disabled in e2e" };
    if (cmd === "llm_providers") return [];
    if (cmd === "ui_state") return null;
    if (cmd === "surfer_available") return false;
    if (cmd === "shell_complete") {
      const prefix = String(args.prefix || "").trim();
      if (!prefix) return [];
      const sess = await mcp.session();
      const propHits = (sess.props || [])
        .map((p) => p.name)
        .filter((n) => n.startsWith(prefix))
        .sort();
      const cmdRes = await mcp.evalTcl(`info commands ::${prefix}*`);
      const cmdHits = cmdRes.ok && cmdRes.result
        ? String(cmdRes.result)
            .split(/\s+/)
            .map((c) => c.replace(/^::/, ""))
            .filter((c) => c && !c.endsWith("_cmd"))
            .sort()
        : [];
      return [...new Set([...cmdHits, ...propHits])];
    }
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
          // Bypass serialization for non-blocking stop, live snapshot, and file trace reads
          // so real mid-prove UI interaction matches Tauri's concurrent backend.
          const concurrent =
            cmd === "shell_stop" ||
            cmd === "read_text" ||
            cmd === "read_trace" ||
            cmd === "read_trace_meta" ||
            cmd === "read_trace_bin" ||
            cmd === "read_trace_window";
          const out = concurrent ? await handleRpc(cmd, args) : await serial(() => handleRpc(cmd, args));
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
    workdir,
    async stop() {
      mcp.close();
      await new Promise((resolve) => server.close(resolve));
      fs.rmSync(workdir, { recursive: true, force: true });
    },
  };
}
