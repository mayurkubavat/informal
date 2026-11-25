// Layer 5 — Real-Backend Headless E2E Smoke Suite
// Drives the real React UI in Playwright connected via `?bridge=` to the REAL
// compiled Rust `informal` binary (`svlang` + CaDiCaL + `informal-fpv` engines)
// — zero `mock.ts`, catching any Rust <-> UI protocol or solver-flow drift!

import fs from "node:fs";
import path from "node:path";
import { expect, test } from "@playwright/test";
import { findInformalBinary, startRealBackendBridge } from "./real-backend-server.mjs";

const HAS_REAL_BIN = Boolean(findInformalBinary());

test.describe("Layer 5 — Real Rust Backend E2E", () => {
  test.skip(!HAS_REAL_BIN, "requires compiled target/{release,debug}/informal binary");

  let bridge: { url: string; workdir: string; stop: () => Promise<void> };

  test.beforeEach(async () => {
    bridge = await startRealBackendBridge();
  });

  test.afterEach(async () => {
    await bridge?.stop();
  });

  test("Real AXI4-Lite template with empty <embedded>: Prove all runs real engines, verifies 4 assumes alive, and renders green Proven rows with Engine/Bound/Time", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);

    // Load AXI4-Lite with `<embedded>` deliberately left current (0 props) via script path input
    const pathInput = page.getByRole("textbox", { name: "Script path" });
    await pathInput.fill("template:axi4lite-empty-embedded");
    await pathInput.press("Enter");

    // Wait for real svlang elaboration + check_protocol synthesis (12 props in task Protocol)
    await expect(page.locator(".strip-summary")).toContainText("12 properties", { timeout: 15_000 });
    await expect(page.locator(".tree")).toContainText("<embedded>");
    await expect(page.locator(".tree")).toContainText("Protocol");
    await expect(page.locator(".env-pill")).toContainText("4 assumes");

    // Switch Effort to Deep (`high`) before proving -> echoes `set_prove_effort high` against real Rust session
    const effortSel = page.getByRole("combobox", { name: "Proof effort" });
    await effortSel.selectOption("high");
    await expect(page.locator(".console")).toContainText("set_prove_effort high");

    // Click Prove all -> must prove real Protocol properties (not no-op on empty <embedded>!)
    const proveBtn = page.getByRole("button", { name: "Prove all" });
    await expect(proveBtn).toContainText("Protocol (8)");
    await proveBtn.click();

    // All 6 assertions prove non-vacuously and both write/read txn covers hit (`6 proven`, `2 covered`, 0 `unreachable`)!
    await expect(page.locator(".strip-summary")).toContainText("6 proven", { timeout: 30_000 });
    await expect(page.locator(".strip-summary")).toContainText("2 covered");
    await expect(page.locator(".strip-summary")).not.toContainText("unreachable");
    await expect(page.locator(".console")).not.toContainText("VACUOUS");

    // Real engines settle Protocol::axi4lite_aw_payload_stable as Proven with green glyph + ∞ + engine + time
    const provenRow = page.locator("table.grid tbody tr", { hasText: "Protocol::axi4lite_aw_payload_stable" });
    await expect(provenRow).toContainText("Proven");
    await expect(provenRow.locator(".st-proven")).toBeVisible();
    await expect(provenRow).toContainText("∞");
    await expect(provenRow).toContainText(/informal-|ric3|abc/);

    // Real 20-step environment check verified all 4 assumptions alive!
    await expect(page.locator(".env-pill.env-alive")).toContainText("Env: alive · 4 assumes");
  });

  test("Real counter example: filter to CEX, then click green Proven a_max in TaskTree -> reveals a_max in PropTable with real engine & ∞ bound", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);

    const pathInput = page.getByRole("textbox", { name: "Script path" });
    await pathInput.fill("examples/01-quickstart/counter/run.jg.tcl");
    await pathInput.press("Enter");

    // Wait for real prove inside counter/run.jg.tcl to settle
    await expect(page.locator(".strip-summary")).toContainText("4 proven", { timeout: 30_000 });
    await expect(page.locator(".strip-summary")).toContainText("1 cex");

    // Filter to CEX via clickable summary pill
    await page.locator(".strip-summary button", { hasText: "1 cex" }).click();
    await expect(page.locator("table.grid tbody tr")).toHaveCount(1);
    await expect(page.getByRole("button", { name: "clear status filter" })).toContainText("Status: CEX");

    // Click a_max under Proven in TaskTree -> UX-2 auto-clears CEX filter & reveals green Proven row!
    await page.locator(".tree-row.leaf", { hasText: "a_max" }).click();
    await expect(page.getByRole("button", { name: "clear status filter" })).toHaveCount(0);
    const maxRow = page.locator("table.grid tbody tr", { hasText: "a_max" });
    await expect(maxRow).toBeVisible();
    await expect(maxRow).toHaveClass(/\bsel\b/);
    await expect(maxRow.locator(".st-proven")).toBeVisible();
    await expect(maxRow).toContainText("∞");
    await expect(maxRow).toContainText(/informal-|ric3|abc/);
  });

  test("Real Signoff effort: vacuous assertion shows amber badge, non-vacuous assertion opens real witness VCD + proof core, and check_assumptions -live runs", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);

    const pathInput = page.getByRole("textbox", { name: "Script path" });
    await pathInput.fill("examples/01-quickstart/counter/run.jg.tcl");
    await pathInput.press("Enter");
    await expect(page.locator(".strip-summary")).toContainText("4 proven", { timeout: 30_000 });

    // Add a vacuous assertion (`1'b0 |-> q == 4'd0`) and a fairness assumption via real Tcl eval
    const prompt = page.getByLabel("Tcl command");
    await prompt.fill("assert -name a_vac_real {1'b0 |-> q == 4'd0}");
    await prompt.press("Enter");
    await expect(page.locator("table.grid tbody tr", { hasText: "a_vac_real" })).toBeVisible();

    // Switch to Signoff (`max`) effort -> real solver generates witness VCD + proof core!
    await page.getByRole("combobox", { name: "Proof effort" }).selectOption("max");
    await expect(page.locator(".console-log")).toContainText("set_prove_effort max");
    await page.getByRole("button", { name: "Prove all" }).click();
    await expect(page.locator(".strip-summary")).toContainText("5 proven", { timeout: 30_000 });

    // Verify real vacuity badge on `a_vac_real`
    const vacRow = page.locator(".table-wrap table.grid tbody tr", { hasText: "a_vac_real" });
    await expect(vacRow.locator(".tag-vacuous")).toContainText("vacuous");
    await vacRow.locator("td").first().click();
    await expect(page.getByRole("tab", { name: "Details: a_vac_real" })).toHaveAttribute("aria-selected", "true");
    await expect(page.locator(".details .vacuity-warn")).toContainText("vacuous");

    // Verify real Proof core + real Witness VCD waveform on `a_step` (an implication assertion with a witness cover)
    const stepRow = page.locator(".table-wrap table.grid tbody tr", { hasText: "a_step" });
    await stepRow.click();
    await expect(page.locator(".details .vacuity-ok")).toHaveText("non vacuous");
    await expect(page.locator(".details .proof-core-summary")).toContainText(/state/);
    await expect(page.locator(".details .proof-core-summary")).toContainText(/input/);
    const openWit = page.locator(".details").getByRole("button", { name: "Open witness" });
    await expect(openWit).toBeVisible();
    await openWit.click();
    await expect(page.locator(".trace-bar .strip-summary")).toContainText("Proven (witness)");
    await expect(page.locator(".trace-bar .strip-summary")).toContainText(/cycles/);

    // Return to Table via ContextBar and run real `check_assumptions -live` from `.env-pill`
    await page.getByRole("region", { name: "Active property context" }).getByRole("button", { name: "Table" }).click();
    await page.locator(".env-pill").click({ button: "right" });
    await page.getByRole("menu", { name: "Environment checks" }).getByRole("menuitem", { name: /Check liveness fairness/ }).click();
    await expect(page.locator(".console-log")).toContainText("check_assumptions -live");
  });

  test("Real FST binary trace streaming: real solver writes sibling .fst on disk, loads binary IFTR over bridge, displays Export VCD (FST), and paints canvas waveforms", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);

    const pathInput = page.getByRole("textbox", { name: "Script path" });
    await pathInput.fill("examples/01-quickstart/counter/run.jg.tcl");
    await pathInput.press("Enter");
    await expect(page.locator(".strip-summary")).toContainText("1 cex", { timeout: 30_000 });

    // Verify real Rust backend wrote a non-empty GTKWave `.fst` sibling in workdir
    const fstFiles = fs
      .readdirSync(bridge.workdir)
      .filter((f) => f.endsWith(".fst") && fs.statSync(path.join(bridge.workdir, f)).size > 0);
    expect(fstFiles.length).toBeGreaterThan(0);

    // Double-click `a_never9` CEX -> loads via `read_trace_meta` (`format: "fst"`) + `read_trace_bin` (IFTR v1)
    await page.locator(".table-wrap table.grid tbody tr", { hasText: "a_never9" }).dblclick();
    await expect(page.getByRole("button", { name: "Export VCD (FST)" })).toBeVisible();

    const canvas = page.locator("canvas.trace-canvas");
    await expect(canvas).toBeVisible();
    const ink = await canvas.evaluate((el) => {
      const cvs = el as HTMLCanvasElement;
      const ctx = cvs.getContext("2d");
      if (!ctx || cvs.width === 0 || cvs.height === 0) return 0;
      const { data, width, height } = ctx.getImageData(0, 0, cvs.width, cvs.height);
      const refIdx = ((height - 2) * width + (width - 2)) * 4;
      const br = data[refIdx];
      const bg = data[refIdx + 1];
      const bb = data[refIdx + 2];
      let diff = 0;
      let total = 0;
      for (let y = 0; y < height; y += 4) {
        for (let x = 0; x < width; x += 4) {
          const i = (y * width + x) * 4;
          if (Math.abs(data[i] - br) + Math.abs(data[i + 1] - bg) + Math.abs(data[i + 2] - bb) > 12) diff++;
          total++;
        }
      }
      return total > 0 ? diff / total : 0;
    });
    expect(ink).toBeGreaterThan(0.01);

    // Click-to-scrub on real FST-backed trace updates cursor cycle cleanly
    const pxText = (await page.locator(".trace-bar .zoom-text").textContent()) || "64px";
    const pxPerCycle = Number.parseFloat(pxText);
    const lanes = page.locator("svg.trace-lanes");
    const box = (await lanes.boundingBox())!;
    await page.mouse.click(box.x + 2.5 * pxPerCycle, box.y + 44);
    await expect(page.locator(".trace-cursor")).toContainText("cycle 2");
  });
});
