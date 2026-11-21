// Layer 4 — Dual-Persona UX User-Journey E2E Tests (Novice + Expert)
// Enforces UX-1..UX-6 across multi-task Formal App templates (`template:axi4lite`),
// filter/selection interactions, assumption visibility, and spatial continuity.

import { expect, test } from "@playwright/test";

test.describe("Dual-Persona UX Journeys (UX-1..UX-6)", () => {
  test("UX-1 + UX-4: Loading template:axi4lite (empty <embedded>, 12 props in Protocol) -> Prove all proves Protocol and shows green Proven rows + alive assumptions", async ({
    page,
  }) => {
    await page.goto("/?welcome=1&fast=1");
    // Novice opens AXI4-Lite template from WelcomePane
    await page.getByRole("button", { name: /AXI4-Lite Bus Protocol VIP/i }).click();

    // Verify Tasks sidebar shows `<embedded>` (0) and `Protocol` (12)
    await expect(page.locator(".tree")).toContainText("<embedded>");
    await expect(page.locator(".tree")).toContainText("Protocol");

    // UX-4: Active assumptions badge visible right away (`4 assumes`)
    await expect(page.locator(".env-pill")).toContainText("4 assumes");

    // UX-6: Guided next-action strip visible on fresh unprocessed load
    await expect(page.locator(".ux-banner.tone-info")).toContainText("8 ready to prove in task Protocol");

    // UX-1: Prove all button shows target badge `Protocol (8)` and actually proves Protocol!
    const proveBtn = page.getByRole("button", { name: "Prove all" });
    await expect(proveBtn).toBeEnabled();
    await expect(proveBtn).toContainText("Protocol (8)");
    await proveBtn.click();

    // Green Proven rows with Engine, ∞ Bound, and Time MUST appear in PropTable
    await expect(page.locator(".strip-summary")).toContainText("6 proven");
    await expect(page.locator(".strip-summary")).toContainText("2 covered");
    const provenRow = page.locator("table.grid tbody tr", { hasText: "Protocol::axi4lite_aw_payload_stable" });
    await expect(provenRow).toBeVisible();
    await expect(provenRow.locator(".st-proven")).toBeVisible();
    await expect(provenRow).toContainText("Proven");
    await expect(provenRow).toContainText("informal-kind");
    await expect(provenRow).toContainText("∞");

    // UX-4: Environment badge upgrades to `Env: alive · 4 assumes`
    await expect(page.locator(".env-pill.env-alive")).toContainText("Env: alive · 4 assumes");
  });

  test("UX-2 + UX-3 + UX-5: Filtering to Unprocessed, proving 1 property, then revealing via banner or TaskTree leaf shows green Proven row in PropTable", async ({
    page,
  }) => {
    await page.goto("/?welcome=1&fast=1");
    await page.getByRole("button", { name: /AXI4-Lite Bus Protocol VIP/i }).click();

    // Filter to Unprocessed via clickable summary pill (UX-3)
    await page.locator(".strip-summary button", { hasText: "8 unprocessed" }).click();
    await expect(page.getByRole("button", { name: "clear status filter" })).toContainText("Status: Unprocessed");
    await expect(page.locator(".filter-count")).toContainText("8 of 12");

    // Prove ONE property via Console while filtered to Unprocessed (exact user screenshot flow!)
    const input = page.locator("#console-input");
    await input.fill("prove -property Protocol::axi4lite_aw_payload_stable");
    await input.press("Enter");

    // UX-5: Newly settled property hidden by Unprocessed filter triggers non-blocking verdict banner!
    const verdictBanner = page.locator(".ux-banner.tone-verdict");
    await expect(verdictBanner).toContainText("Protocol::axi4lite_aw_payload_stable → Proven");
    await expect(verdictBanner).toContainText("hidden by Unprocessed filter");

    // UX-2: Clicking the Proven leaf in TaskTree MUST clear the conflicting filter and reveal the green Proven row!
    await page.locator(".tree-row.leaf", { hasText: "Protocol::axi4lite_aw_payload_stable" }).click();
    await expect(page.getByRole("button", { name: "clear status filter" })).toHaveCount(0);
    const row = page.locator("table.grid tbody tr", { hasText: "Protocol::axi4lite_aw_payload_stable" });
    await expect(row).toBeVisible();
    await expect(row).toHaveClass(/\bsel\b/);
    await expect(row.locator(".st-proven")).toBeVisible();
    await expect(row).toContainText("informal-kind");
    await expect(row).toContainText("∞");
  });

  test("UX-5: Clicking 'Show in table' on verdict notice reveals newly settled property directly", async ({
    page,
  }) => {
    await page.goto("/?welcome=1&fast=1");
    await page.getByRole("button", { name: /AXI4-Lite Bus Protocol VIP/i }).click();
    await page.locator(".strip-summary button", { hasText: "8 unprocessed" }).click();

    const input = page.locator("#console-input");
    await input.fill("prove -property Protocol::axi4lite_aw_valid_hold");
    await input.press("Enter");

    const banner = page.locator(".ux-banner.tone-verdict");
    await expect(banner).toBeVisible();
    await banner.getByRole("button", { name: "Show in table" }).click();

    const row = page.locator("table.grid tbody tr", { hasText: "Protocol::axi4lite_aw_valid_hold" });
    await expect(row).toBeVisible();
    await expect(row.locator(".st-proven")).toBeVisible();
  });

  test("UX-3: Clicking Env assumptions pill filters to active assumptions and toggles back cleanly", async ({
    page,
  }) => {
    await page.goto("/?welcome=1&fast=1");
    await page.getByRole("button", { name: /AXI4-Lite Bus Protocol VIP/i }).click();

    await page.locator(".env-pill").click();
    await expect(page.getByRole("button", { name: "clear status filter" })).toContainText("Status: Assumed");
    await expect(page.locator("table.grid tbody tr")).toHaveCount(4);

    // Dismiss chip to restore all 12 rows
    await page.getByRole("button", { name: "clear status filter" }).click();
    await expect(page.locator("table.grid tbody tr")).toHaveCount(12);
  });
});
