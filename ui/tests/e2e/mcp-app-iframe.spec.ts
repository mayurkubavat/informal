// Real Host-Embedded SEP-1865 MCP App (`crates/informal-mcp/assets/workbench.html`)
// `<iframe>` JSON-RPC 2.0 `postMessage` contract test.
//
// Unlike `/?mcpApp=1` loaded top-level, this mounts the ACTUAL compiled single-file
// `workbench.html` bundle inside a sandboxed `<iframe sandbox="allow-scripts">`
// hosted by an Antigravity-compatible SEP-1865 parent host, verifying:
// 1. Real `ui/initialize` -> `ui/notifications/initialized` handshake
// 2. Host theme & CSS variable synchronization (`ui/notifications/host-context-changed`)
// 3. Host `ui/notifications/tool-result` session hydration into the workbench table
// 4. Bidirectional `tools/call` RPC from inside the iframe (`why_cex`, `schematic_get`)
// 5. Responsive 460px Antigravity side-pane layout inside the iframe with zero overflow

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const WORKBENCH_HTML_PATH = path.resolve(__dirname, "../../../crates/informal-mcp/assets/workbench.html");

const HOST_ORIGIN = "http://mcp-host.local";

function buildHostPageHtml(iframeWidth: number, iframeHeight: number): string {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>SEP-1865 MCP App Host Harness</title>
  <style>
    html, body { margin: 0; padding: 0; width: 100%; height: 100%; background: #111; }
    iframe { display: block; border: 0; width: ${iframeWidth}px; height: ${iframeHeight}px; }
  </style>
</head>
<body>
  <iframe id="mcp-frame" sandbox="allow-scripts" src="${HOST_ORIGIN}/workbench.html"></iframe>
  <script>
    const frame = document.getElementById("mcp-frame");
    const log = [];
    let initialized = false;
    let initResolve;
    const initReady = new Promise((r) => { initResolve = r; });

    function postToApp(msg) {
      frame.contentWindow.postMessage(msg, "*");
    }

    window.addEventListener("message", (ev) => {
      const msg = ev.data;
      if (!msg || msg.jsonrpc !== "2.0") return;
      log.push(msg);

      if (msg.method === "ui/initialize" && msg.id != null) {
        postToApp({
          jsonrpc: "2.0",
          id: msg.id,
          result: {
            protocolVersion: "2026-01-26",
            hostInfo: { name: "AntigravityTestHost", version: "1.0.0" },
            hostCapabilities: {
              updateModelContext: { text: {} },
              serverTools: {},
            },
            hostContext: {
              theme: "light",
              styles: {
                variables: {
                  "--color-background-primary": "#ffffff",
                  "--color-text-primary": "#1a1a1a",
                },
              },
            },
          },
        });
        return;
      }

      if (msg.method === "ui/notifications/initialized") {
        initialized = true;
        initResolve();
        return;
      }

      if (msg.method === "tools/call" && msg.id != null) {
        const name = msg.params?.name;
        const args = msg.params?.arguments || {};
        if (name === "why_cex") {
          postToApp({
            jsonrpc: "2.0",
            id: msg.id,
            result: {
              isError: false,
              structuredContent: {
                schema: "informal.why_cex/1",
                property: args.name || "a_never9",
                status: "cex",
                failure_frame: 11,
                total_frames: 12,
                static_cone_signals: 4,
                active_slice_signals: 2,
                active_signals: ["counter.en", "counter.q"],
                events: [
                  { frame: 1, signal: "counter.en", kind: "input", width: 1, value: "1'b1", reason: "enable_high" },
                  { frame: 11, signal: "counter.q", kind: "state", width: 4, value: "4'h9", prev_value: "4'h8", reason: "violation" },
                ],
              },
            },
          });
          return;
        }
        if (name === "trace_get") {
          postToApp({
            jsonrpc: "2.0",
            id: msg.id,
            result: {
              isError: false,
              structuredContent: {
                schema: "informal.trace/1",
                frames: 3,
                unit: "cycle",
                first: 0,
                last: 2,
                hit_at: [2],
                property: { name: args.name || "LowPower::lp_iso_before_pwroff_PD_TIM" },
                signals: [
                  { name: "subsys.u_pmu.tim_pwr_en_q", width: 1, values: ["1", "1", "0"] },
                  { name: "subsys.u_pmu.tim_iso_en_q", width: 1, values: ["0", "0", "0"] },
                  { name: "subsys.PRDATA", width: 32, values: ["00000011", "00000011", "00008000"] },
                  { name: "lp.pd.PD_AON.on", width: 1, values: ["1", "1", "1"] },
                  { name: "lp.pd.PD_TIM.on", width: 1, values: ["1", "1", "0"] },
                  { name: "lp.pd.PD_TIM.iso", width: 1, values: ["0", "0", "0"] },
                  { name: "lp.pd.PD_TIM.corrupt", width: 1, values: ["0", "0", "1"] },
                ],
              },
            },
          });
          return;
        }
        if (name === "design_source") {
          postToApp({
            jsonrpc: "2.0",
            id: msg.id,
            result: {
              isError: false,
              structuredContent: {
                schema: "informal.source/1",
                file: args.file || "counter.sv",
                first_line: 1,
                last_line: 3,
                total_lines: 3,
                lines: [
                  "module counter(input logic clk, input logic rst_n, output logic [3:0] q);",
                  "  always_ff @(posedge clk) q <= !rst_n ? 4'd0 : q + 4'd1;",
                  "endmodule",
                ],
              },
            },
          });
          return;
        }
        if (name === "schematic_get") {
          postToApp({
            jsonrpc: "2.0",
            id: msg.id,
            result: {
              isError: false,
              structuredContent: {
                schema: "informal.schematic/1",
                property: args.name || "a_never9",
                max_depth: args.max_depth ?? 2,
                truncated: false,
                nodes: [
                  { id: "in_en", label: "en", kind: "input", width: 1, distance: 2, signal: "counter.en", causal: true, hotspot: false, expandable: false, ports: [{ id: "in_en:out", label: "out", dir: "out" }] },
                  { id: "reg_q", label: "q", kind: "register", width: 4, distance: 1, signal: "counter.q", causal: true, hotspot: false, expandable: false, ports: [{ id: "reg_q:in", label: "d", dir: "in" }, { id: "reg_q:out", label: "q", dir: "out" }] },
                  { id: "bad_0", label: "a_never9", kind: "target", width: 1, distance: 0, signal: "informal.a_never9", causal: true, hotspot: false, expandable: false, ports: [{ id: "bad_0:in", label: "in", dir: "in" }] },
                ],
                edges: [
                  { id: "e1", source: "in_en", source_port: "in_en:out", target: "reg_q", target_port: "reg_q:in", width: 1, causal: true },
                  { id: "e2", source: "reg_q", source_port: "reg_q:out", target: "bad_0", target_port: "bad_0:in", width: 4, causal: true },
                ],
              },
            },
          });
          return;
        }
        postToApp({
          jsonrpc: "2.0",
          id: msg.id,
          result: { isError: false, structuredContent: { schema: "informal.ack/1", tool: name } },
        });
      }
    });

    window.__mcpHost = {
      waitReady: () => initReady,
      isInitialized: () => initialized,
      getLog: () => log,
      sendTheme(theme) {
        postToApp({
          jsonrpc: "2.0",
          method: "ui/notifications/host-context-changed",
          params: { theme },
        });
      },
      sendToolResult(structuredContent) {
        postToApp({
          jsonrpc: "2.0",
          method: "ui/notifications/tool-result",
          params: {
            isError: false,
            structuredContent,
            content: [{ type: "text", text: JSON.stringify(structuredContent) }],
          },
        });
      },
    };
  </script>
</body>
</html>`;
}

test.describe("SEP-1865 Real Built MCP App Iframe Contract (workbench.html)", () => {
  test("mounts real workbench.html in sandboxed iframe, completes handshake, syncs host theme, hydrates tool-result, and sends tools/call RPC", async ({
    page,
  }) => {
    expect(fs.existsSync(WORKBENCH_HTML_PATH), `missing ${WORKBENCH_HTML_PATH}`).toBe(true);
    const workbenchHtml = fs.readFileSync(WORKBENCH_HTML_PATH, "utf8");

    const pageErrors: string[] = [];
    page.on("pageerror", (e) => pageErrors.push(String(e)));

    await page.route(`${HOST_ORIGIN}/host.html`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html; charset=utf-8", body: buildHostPageHtml(1200, 800) }),
    );
    await page.route(`${HOST_ORIGIN}/workbench.html`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html; charset=utf-8", body: workbenchHtml }),
    );

    await page.goto(`${HOST_ORIGIN}/host.html`);
    await page.evaluate(() => (window as unknown as { __mcpHost: { waitReady: () => Promise<void> } }).__mcpHost.waitReady());

    const frame = page.frameLocator("#mcp-frame");
    await expect(frame.locator(".app.mcp-app-mode")).toBeVisible();
    await expect(frame.getByRole("menubar", { name: "Main menu" })).toHaveCount(0);

    // 1. Verify live host theme push (light -> dark -> light) updates :root[data-theme] inside iframe
    await page.evaluate(() => (window as unknown as { __mcpHost: { sendTheme: (t: string) => void } }).__mcpHost.sendTheme("dark"));
    await expect(frame.locator(":root")).toHaveAttribute("data-theme", "dark");
    await page.evaluate(() => (window as unknown as { __mcpHost: { sendTheme: (t: string) => void } }).__mcpHost.sendTheme("light"));
    await expect(frame.locator(":root")).toHaveAttribute("data-theme", "light");

    // 2. Push `informal.workbench/1` tool-result from host -> hydrates iframe PropTable
    await page.evaluate(() => {
      const host = (window as unknown as { __mcpHost: { sendToolResult: (v: unknown) => void } }).__mcpHost;
      host.sendToolResult({
        schema: "informal.workbench/1",
        focus_property: "a_never9",
        view: "table",
        session: {
          design: { files: ["counter.sv"], top: "counter", defines: [], hierarchy: [] },
          elaborated: true,
          clocks: [{ signal: "clk", edge: "posedge" }],
          reset: "!rst_n",
          props: [
            {
              name: "a_max",
              kind: "assert",
              expr: "q <= 4'd9",
              enabled: true,
              status: { status: "proven", engine: "informal-ic3", ms: 22 },
            },
            {
              name: "a_never9",
              kind: "assert",
              expr: "q != 4'd9",
              enabled: true,
              trace: "informal_work/a_never9.vcd",
              status: { status: "cex", engine: "informal-bmc", depth: 11, ms: 18 },
            },
          ],
          time_limit: { secs: 60, nanos: 0 },
          effort: "standard",
          orchestration: "auto",
          workdir: "informal_work",
        },
      });
    });

    await expect(frame.locator("table.grid tbody tr", { hasText: "a_never9" })).toBeVisible();
    await expect(frame.locator("table.grid tbody tr", { hasText: "a_max" })).toBeVisible();

    // 3. Trigger bidirectional `tools/call` (`schematic_get`) from inside iframe -> host receives real JSON-RPC!
    await frame.locator("table.grid tbody tr", { hasText: "a_never9" }).click();
    await frame.getByRole("button", { name: "COI & causal schematic" }).click();

    await expect
      .poll(async () => {
        const log = await page.evaluate(() =>
          (window as unknown as { __mcpHost: { getLog: () => { method?: string; params?: { name?: string } }[] } }).__mcpHost.getLog(),
        );
        return log.some((m) => m.method === "tools/call" && m.params?.name === "schematic_get");
      })
      .toBe(true);

    // Schematic renders inside the iframe from host's JSON-RPC reply
    await expect(frame.locator(".schematic-pane canvas")).toBeVisible();

    // 4. Push `open_workbench` with `view: "trace"` + `focus_property` -> immediately opens Trace tab,
    //    calls `tools/call` `trace_get`, renders real `<canvas>` waveform + `lp.pd.PD_TIM` swimlanes!
    await page.evaluate(() => {
      const host = (window as unknown as { __mcpHost: { sendToolResult: (v: unknown) => void } }).__mcpHost;
      host.sendToolResult({
        schema: "informal.workbench/1",
        focus_property: "LowPower::lp_iso_before_pwroff_PD_TIM",
        view: "trace",
        session: {
          design: { files: ["counter.sv"], top: "subsys", defines: [], hierarchy: [] },
          elaborated: true,
          clocks: [{ signal: "PCLK", edge: "posedge" }],
          reset: "!PRESETn",
          props: [
            {
              name: "LowPower::lp_iso_before_pwroff_PD_TIM",
              kind: "assert",
              task: "LowPower",
              expr: "(!u_pmu.tim_pwr_en_q) |-> u_pmu.tim_iso_en_q",
              enabled: true,
              trace: "/tmp/LowPower__lp_iso_before_pwroff_PD_TIM.vcd",
              status: { status: "cex", engine: "informal-kind", depth: 2, ms: 22 },
            },
          ],
          time_limit: { secs: 60, nanos: 0 },
          effort: "standard",
          orchestration: "auto",
          workdir: "/tmp",
        },
      });
    });

    await expect
      .poll(async () => {
        const log = await page.evaluate(() =>
          (window as unknown as { __mcpHost: { getLog: () => { method?: string; params?: { name?: string } }[] } }).__mcpHost.getLog(),
        );
        return log.some((m) => m.method === "tools/call" && m.params?.name === "trace_get");
      })
      .toBe(true);

    const traceCanvas = frame.locator("canvas.trace-canvas");
    await expect(traceCanvas).toBeVisible();
    await expect(frame.locator(".trace-names")).toContainText("subsys.u_pmu.tim_pwr_en_q");
    await expect(frame.getByRole("region", { name: "Power domain swimlanes" })).toContainText("PD_TIM: CORRUPT");

    // 5. Switch to Source view inside iframe -> fetches real RTL via `design_source`
    await frame.getByRole("button", { name: /Source view/i }).click();
    await expect(frame.getByRole("textbox", { name: "Source of counter.sv" })).toContainText("always_ff @(posedge clk)");
    expect(pageErrors).toEqual([]);
  });

  test("real workbench.html adapts cleanly to a 460px Antigravity side-pane iframe with zero overflow", async ({ page }) => {
    const workbenchHtml = fs.readFileSync(WORKBENCH_HTML_PATH, "utf8");
    await page.route(`${HOST_ORIGIN}/host-narrow.html`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html; charset=utf-8", body: buildHostPageHtml(460, 760) }),
    );
    await page.route(`${HOST_ORIGIN}/workbench.html`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html; charset=utf-8", body: workbenchHtml }),
    );

    await page.goto(`${HOST_ORIGIN}/host-narrow.html`);
    await page.evaluate(() => (window as unknown as { __mcpHost: { waitReady: () => Promise<void> } }).__mcpHost.waitReady());

    const frame = page.frameLocator("#mcp-frame");
    await expect(frame.getByRole("button", { name: "Toggle tasks sidebar" })).toBeVisible();
    const geom = await frame.locator(":root").evaluate(() => ({
      scrollW: document.documentElement.scrollWidth,
      clientW: document.documentElement.clientWidth,
    }));
    expect(geom.scrollW).toBeLessThanOrEqual(geom.clientW);
  });
});
