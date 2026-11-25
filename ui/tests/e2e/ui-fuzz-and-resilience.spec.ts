// Pillar 4 (E2E) — Seeded Stateful UI Chaos Fuzzer, Out-of-Order Async Race Guard,
// Fault Injection (Corrupt Storage / Broken Trace) & DOM Node-Count Leak Bound.

import http from "node:http";
import { expect, test } from "@playwright/test";

function makeRng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function tinyVcd(sigName: string, values: string[]): string {
  const lines = [
    "$timescale 1ns $end",
    `$comment informal trace of demo: ${values.length} cycles $end`,
    "$scope module dut $end",
    "$var wire 1 ! clk $end",
    `$var wire 4 " ${sigName} $end`,
    "$upscope $end",
    "$enddefinitions $end",
  ];
  for (let i = 0; i < values.length; i++) {
    lines.push(`#${i * 10}`, "1!", `b${values[i]} "`, `#${i * 10 + 5}`, "0!");
  }
  return `${lines.join("\n")}\n`;
}

async function startRaceMockBridge() {
  const session = {
    design: { files: ["dut.sv"], top: "dut", defines: [], hierarchy: [] },
    elaborated: true,
    clocks: [{ signal: "clk", edge: "posedge" }],
    reset: "!rst_n",
    props: [
      {
        name: "a_slow_cex",
        kind: "assert",
        expr: "slow_sig != 4'h9",
        enabled: true,
        trace: "slow.vcd",
        status: { status: "cex", engine: "informal-bmc", depth: 4, ms: 12 },
      },
      {
        name: "a_fast_cex",
        kind: "assert",
        expr: "fast_sig != 4'h5",
        enabled: true,
        trace: "fast.vcd",
        status: { status: "cex", engine: "informal-bmc", depth: 3, ms: 9 },
      },
      {
        name: "a_broken_cex",
        kind: "assert",
        expr: "bad_sig != 4'h1",
        enabled: true,
        trace: "missing.vcd",
        status: { status: "cex", engine: "informal-bmc", depth: 2, ms: 5 },
      },
    ],
    time_limit: { secs: 60, nanos: 0 },
    effort: "standard",
    orchestration: "auto",
    workdir: "informal_work",
  };

  let slowTraceServed = false;
  const server = http.createServer((req, res) => {
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Methods", "POST, OPTIONS");
    res.setHeader("Access-Control-Allow-Headers", "content-type");
    if (req.method === "OPTIONS") {
      res.writeHead(204);
      res.end();
      return;
    }
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", async () => {
      const { cmd, args } = JSON.parse(body || "{}");
      const send = (val: unknown, status = 200) => {
        res.writeHead(status, { "content-type": status === 200 ? "application/json" : "text/plain" });
        res.end(status === 200 ? JSON.stringify(val) : String(val));
      };
      if (cmd === "snapshot") return send({ ok: true, result: "", session });
      if (cmd === "current_flavor") return send("jg");
      if (cmd === "read_text") return send("module dut(input clk, input rst_n); endmodule\n");
      if (cmd === "read_trace_meta" || cmd === "read_trace_bin") {
        // Force fallback to `read_trace` so we precisely control async latency per trace file
        return send(null);
      }
      if (cmd === "read_trace") {
        const p = String(args?.path || "");
        if (p === "slow.vcd") {
          await new Promise((r) => setTimeout(r, 340));
          slowTraceServed = true;
          return send(tinyVcd("slow_sig", ["0000", "0001", "0010", "1001", "1001"]));
        }
        if (p === "fast.vcd") {
          await new Promise((r) => setTimeout(r, 15));
          return send(tinyVcd("fast_sig", ["0000", "0011", "0101", "0101"]));
        }
        return send("ENOENT: simulated corrupt/missing trace file", 500);
      }
      if (cmd === "ai_status") return send({ mode: "propose", socket: null, proposals: [] });
      if (cmd === "llm_status") return send({ provider: "none", model: "", ok: false, error: "off" });
      if (cmd === "llm_providers") return send([]);
      if (cmd === "surfer_available") return send(false);
      return send(null);
    });
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address() as { port: number };
  return {
    url: `http://127.0.0.1:${addr.port}`,
    wasSlowServed: () => slowTraceServed,
    stop: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

test.describe("Stateful UI Fuzzer, Async Race Guard, Fault Injection & DOM Soak", () => {
  test("50-step deterministic seeded UI chaos fuzzer incurs zero pageerror, zero console.error, and zero overflow", async ({
    page,
  }) => {
    const pageErrors: string[] = [];
    const consoleErrors: string[] = [];
    page.on("pageerror", (e) => pageErrors.push(String(e)));
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });

    await page.goto("/?fast=1");
    await page.getByRole("button", { name: "Prove all" }).click();
    await expect(page.locator("footer.statusbar")).toContainText("Idle");

    const rng = makeRng(0x5eed1800);
    const presets = ["triage", "cex-debug", "split", "signoff"] as const;
    const dockTabs = ["Console", "Details", "Messages", "Log"] as const;

    for (let step = 0; step < 50; step++) {
      const op = Math.floor(rng() * 10);
      if (op === 0) {
        const preset = presets[Math.floor(rng() * presets.length)];
        await page.getByRole("combobox", { name: "Layout" }).selectOption(preset);
      } else if (op === 1) {
        // Select a leaf in TaskTree (UX-2)
        const leaves = page.locator(".tree-row.leaf");
        const count = await leaves.count();
        if (count > 0) {
          const leaf = leaves.nth(Math.floor(rng() * count));
          if (await leaf.isVisible()) await leaf.click();
        }
      } else if (op === 2) {
        // Toggle a summary status filter pill if visible
        const pills = page.locator(".strip-summary button");
        const count = await pills.count();
        if (count > 0) {
          const pill = pills.nth(Math.floor(rng() * count));
          if (await pill.isVisible()) await pill.click();
        }
      } else if (op === 3) {
        // Open/close Command Palette via keyboard
        await page.keyboard.press("Control+k");
        await page.keyboard.press("Escape");
      } else if (op === 4) {
        // Open/close Shortcut Sheet via `?` / `Escape` (when not in editable)
        await page.locator("footer.statusbar").click();
        await page.keyboard.press("?");
        await page.keyboard.press("Escape");
      } else if (op === 5) {
        // Switch bottom dock tab
        const label = dockTabs[Math.floor(rng() * dockTabs.length)];
        const tab = page.locator(".bottom .tab", { hasText: label }).first();
        if (await tab.isVisible()) await tab.click();
      } else if (op === 6) {
        // Toggle Trace or Schematic via toolbar buttons
        const btn = rng() < 0.5
          ? page.getByRole("button", { name: /Visualize trace/i }).first()
          : page.getByRole("button", { name: "COI & causal schematic" }).first();
        if ((await btn.isVisible()) && (await btn.isEnabled())) await btn.click();
      } else if (op === 7) {
        // Navigate back via Alt+ArrowLeft
        await page.locator("footer.statusbar").click();
        await page.keyboard.press("Alt+ArrowLeft");
      } else if (op === 8) {
        // Open Source view toggle (`F3`)
        await page.keyboard.press("F3");
      } else {
        // Clear any active status filter chip if present
        const chip = page.getByRole("button", { name: "clear status filter" }).first();
        if (await chip.isVisible()) await chip.click();
      }

      // Check zero horizontal overflow on every 5th step + final step
      if (step % 5 === 4) {
        const geom = await page.evaluate(() => ({
          scrollW: document.documentElement.scrollWidth,
          clientW: document.documentElement.clientWidth,
        }));
        expect(geom.scrollW, `overflow at fuzz step ${step}`).toBeLessThanOrEqual(geom.clientW);
      }
    }

    expect(pageErrors).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });

  test("out-of-order async trace responses never let a slower earlier trace clobber the active trace, and missing trace fails gracefully", async ({
    page,
  }) => {
    const bridge = await startRaceMockBridge();
    try {
      const pageErrors: string[] = [];
      page.on("pageerror", (e) => pageErrors.push(String(e)));

      await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
      await expect(page.locator(".table-wrap table.grid tbody tr", { hasText: "a_slow_cex" })).toBeVisible();

      // 1. Open slow trace (340ms latency) then immediately pivot to fast trace (15ms latency)
      await page.locator(".table-wrap table.grid tbody tr", { hasText: "a_slow_cex" }).dblclick();
      await page.waitForTimeout(30);
      await page.locator(".tree-row.leaf", { hasText: "a_fast_cex" }).click();

      // Fast trace renders `dut.fast_sig`
      await expect(page.locator(".trace-names")).toContainText("dut.fast_sig");

      // Wait until the slow trace's HTTP response has definitely resolved
      await expect.poll(() => bridge.wasSlowServed(), { timeout: 2_000 }).toBe(true);
      await page.waitForTimeout(80);

      // Late slow.vcd reply MUST be discarded (still displaying `dut.fast_sig`, NOT `dut.slow_sig`)
      await expect(page.locator(".trace-names")).toContainText("dut.fast_sig");
      await expect(page.locator(".trace-names")).not.toContainText("dut.slow_sig");

      // 2. Fault injection: missing/corrupt trace shows inline error without crashing app
      await page.locator(".tree-row.leaf", { hasText: "a_broken_cex" }).click();
      await expect(page.locator(".center .empty")).toContainText("Could not read missing.vcd");
      expect(pageErrors).toEqual([]);
    } finally {
      await bridge.stop();
    }
  });

  test("corrupt localStorage layout/recent JSON recovers cleanly on boot without errors", async ({ page }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (e) => pageErrors.push(String(e)));

    await page.addInitScript(() => {
      try {
        localStorage.setItem("informal-layout", "{corrupt-json!!!");
        localStorage.setItem("informal-recent", "not-an-array");
      } catch {
        // ignore
      }
    });

    await page.goto("/?fast=1");
    await expect(page.locator(".table-wrap table.grid tbody tr:not(.empty-row)")).toHaveCount(6);
    await expect(page.locator("aside.left")).toBeVisible();
    await expect(page.locator("section.bottom")).toBeVisible();
    expect(pageErrors).toEqual([]);
  });

  test("DOM node count stays bounded across 8 repeated Trace open/close and Dockview preset toggles", async ({
    page,
  }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (e) => pageErrors.push(String(e)));

    await page.goto("/?fast=1");
    await page.getByRole("button", { name: "Prove all" }).click();
    await expect(page.locator("footer.statusbar")).toContainText("Idle");

    let baselineNodes = 0;
    for (let i = 0; i < 8; i++) {
      await page.getByRole("combobox", { name: "Layout" }).selectOption("cex-debug");
      await expect(page.getByTestId("dockview-workspace")).toBeVisible();

      await page.getByRole("combobox", { name: "Layout" }).selectOption("triage");
      await page.locator(".table-wrap table.grid tbody tr", { hasText: "a_never9" }).dblclick();
      await expect(page.locator("canvas.trace-canvas")).toBeVisible();

      // Close Trace tab via its × button
      await page.locator(".tab-close").first().click();
      await expect(page.locator(".table-wrap table.grid")).toBeVisible();

      if (i === 1) {
        baselineNodes = await page.evaluate(() => document.querySelectorAll("*").length);
      }
    }

    const finalNodes = await page.evaluate(() => document.querySelectorAll("*").length);
    expect(finalNodes).toBeLessThan(2500);
    expect(finalNodes - baselineNodes).toBeLessThan(250);
    expect(pageErrors).toEqual([]);
  });
});
