// Layer 5 — Real-Backend Headless E2E Smoke Suite
// Drives the real React UI in Playwright connected via `?bridge=` to the REAL
// compiled Rust `informal` binary (`svlang` + CaDiCaL + `informal-fpv` engines)
// — zero `mock.ts`, catching any Rust <-> UI protocol or solver-flow drift!

import { expect, test } from "@playwright/test";
import { findInformalBinary, startRealBackendBridge } from "./real-backend-server.mjs";

const HAS_REAL_BIN = Boolean(findInformalBinary());

test.describe("Layer 5 — Real Rust Backend E2E", () => {
  test.skip(!HAS_REAL_BIN, "requires compiled target/{release,debug}/informal binary");

  let bridge: { url: string; stop: () => Promise<void> };

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
});
