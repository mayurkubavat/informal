// Layer 4 — Cross-Pane UX-7..UX-9 Workflows, Formal Apps, Accessibility & 4-Viewport Geometry

import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const rows = (page: Page) => page.locator(".table-wrap table.grid tbody tr:not(.empty-row)");
const row = (page: Page, name: string) => rows(page).filter({ hasText: name });

async function provedSession(page: Page, query = "fast=1") {
  await page.goto(`/?${query}`);
  await expect(rows(page)).toHaveCount(6);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}

test.describe("Layer 4 — Cross-Pane Triage, Proof Trust, Formal Apps & Responsive Accessibility", () => {
  test("cross-pane CEX triage synchronizes ContextBar cycle across Trace -> Why CEX -> Source -> Cone -> 3-Pane CEX and restores filters on Back", async ({
    page,
  }) => {
    await provedSession(page);

    // Filter to CEX first so Back history verifies filter restoration
    await page.locator(".strip-summary button", { hasText: "1 cex" }).click();
    await expect(rows(page)).toHaveCount(1);

    // Double-click CEX row -> opens TraceView with ContextBar at cycle 11
    await row(page, "a_never9").dblclick();
    const ctxBar = page.getByRole("region", { name: "Active property context" });
    await expect(ctxBar).toBeVisible();
    await expect(ctxBar).toContainText("a_never9");
    await expect(ctxBar).toContainText("@C11");

    // Open Why CEX slice and jump to causal cycle @1 -> ContextBar updates live to @C1
    await page.getByRole("button", { name: "Why CEX" }).click();
    await expect(page.getByRole("region", { name: "Causal root-cause slice" })).toBeVisible();
    await page.getByRole("button", { name: "@1 en: 1'b1" }).click();
    await expect(ctxBar).toContainText("@C1");

    // Pivot across Source -> Cone -> 3-Pane CEX from ContextBar
    const pivots = ctxBar.getByRole("group", { name: "Switch property view" });
    await pivots.getByRole("button", { name: "Source" }).click();
    await expect(pivots.getByRole("button", { name: "Source" })).toHaveAttribute("aria-pressed", "true");
    await expect(ctxBar).toContainText("a_never9");

    await pivots.getByRole("button", { name: "Cone" }).click();
    await expect(pivots.getByRole("button", { name: "Cone" })).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator(".schematic-pane")).toBeVisible();

    await pivots.getByRole("button", { name: "3-Pane CEX" }).click();
    await expect(pivots.getByRole("button", { name: "3-Pane CEX" })).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator(".workbench-dockview")).toBeVisible();

    // Return to Table via ContextBar `Table` pill -> restores Table with `Status: CEX` filter still active!
    await pivots.getByRole("button", { name: "Table" }).click();
    await expect(ctxBar).toHaveCount(0);
    await expect(page.getByRole("button", { name: "clear status filter" })).toContainText("Status: CEX");
    await expect(row(page, "a_never9")).toHaveAttribute("aria-selected", "true");

    // Also verify keyboard `F3` -> `Alt+ArrowLeft` restores prior view & filter
    await page.keyboard.press("F3");
    await expect(ctxBar).toBeVisible();
    await page.keyboard.press("Alt+ArrowLeft");
    await expect(ctxBar).toHaveCount(0);
    await expect(page.getByRole("button", { name: "clear status filter" })).toContainText("Status: CEX");
  });

  test("Signoff effort surfaces vacuous proof badge, non-vacuous Proof Core + Witness waveform, -live fairness check, and signal stopat", async ({
    page,
  }) => {
    await page.goto("/?fast=1");
    await expect(rows(page)).toHaveCount(6);

    // Add a vacuous assertion via Tcl console
    const prompt = page.getByLabel("Tcl command");
    await prompt.fill("assert -name a_vac {1'b0 |-> q == 4'd0}");
    await prompt.press("Enter");
    await expect(rows(page)).toHaveCount(7);

    // Select Signoff (`max`) effort and run Prove all
    await page.getByRole("combobox", { name: "Proof effort" }).selectOption("max");
    await expect(page.locator(".console-log")).toContainText("set_prove_effort max");
    await page.getByRole("button", { name: "Prove all" }).click();
    await expect(page.locator("footer.statusbar")).toContainText("Idle");

    // `a_vac` shows amber `⚠ vacuous` badge in PropTable, TaskTree, and Details
    await expect(row(page, "a_vac").locator(".tag-vacuous")).toContainText("vacuous");
    await expect(page.locator(".tree-row.leaf", { hasText: "a_vac" }).locator(".tag-vacuous")).toBeVisible();
    await row(page, "a_vac").click();
    await expect(page.locator(".details .vacuity-warn")).toContainText("vacuous");

    // `a_max` is non-vacuous and shows Proof core + Open witness button
    await row(page, "a_max").click();
    await expect(page.locator(".details .vacuity-ok")).toHaveText("non vacuous");
    await expect(page.locator(".details .proof-core-summary")).toContainText("1 assume (m_en)");
    await expect(page.locator(".details .proof-core-summary")).toContainText("1 state");
    await expect(page.locator(".details .proof-core-summary")).toContainText("1 input");
    await page.locator(".details").getByRole("button", { name: "Open witness" }).click();
    await expect(page.locator(".trace-bar .strip-summary")).toContainText("Proven (witness)");

    // Return to Table and run `check_assumptions -live` from `.env-pill` context menu
    await page.getByRole("region", { name: "Active property context" }).getByRole("button", { name: "Table" }).click();
    await page.locator(".env-pill").click({ button: "right" });
    const envMenu = page.getByRole("menu", { name: "Environment checks" });
    await expect(envMenu).toBeVisible();
    await envMenu.getByRole("menuitem", { name: /Check liveness fairness/ }).click();
    await expect(page.locator(".console-log")).toContainText("informal[jg]> check_assumptions -live");

    // Switch left sidebar to Design tab and right-click signal `q` -> `stopat q`
    await page.locator("aside.left").getByRole("tab", { name: "Design" }).click();
    const qSig = page.locator(".sig-row", { hasText: /^q/ }).first();
    await qSig.click({ button: "right" });
    const sigMenu = page.getByRole("menu", { name: "Signal actions for counter.q" });
    await expect(sigMenu.getByRole("menuitem", { name: /Cut driver/ })).toContainText("stopat q");
    await sigMenu.getByRole("menuitem", { name: /Cut driver/ }).click();
    await expect(page.locator(".console-log")).toContainText("informal[jg]> stopat q");
    await expect(qSig.locator(".tag-cut")).toHaveText("cut");
  });

  test("exercises all 6 FormalApps tabs (COV, X-Prop, SPV, SEC, CSR/Conn, Bus VIP) and navigates to synthesized tasks", async ({
    page,
  }) => {
    await page.goto("/?fast=1");
    await expect(rows(page)).toHaveCount(6);

    // Pre-populate SEC and CSR/Conn via Console so their tables & task buttons are live
    const prompt = page.getByLabel("Tcl command");
    await prompt.fill("check_sec -setup -spec a.sv -imp b.sv");
    await prompt.press("Enter");
    await prompt.fill("check_csr -load regs.hjson");
    await prompt.press("Enter");
    await expect(page.locator(".console-log")).toContainText("CSR::ctrl_reset");

    // Open Formal Apps dock
    await page.getByRole("menuitem", { name: "Tools", exact: true }).click();
    await page.getByRole("menuitem", { name: "Formal apps" }).click();
    const apps = page.locator(".formal-apps");
    await expect(apps).toBeVisible();

    // 1. Coverage (`cov`)
    await apps.getByRole("button", { name: "Measure COI & UNR" }).click();
    await expect(apps).toContainText("counter.br_inc");

    // 2. X-Prop (`xprop`)
    await apps.locator(".apps-tab", { hasText: "X-Prop" }).click();
    await apps.getByRole("button", { name: "Prove X-Prop" }).click();
    await expect(apps).toContainText("counter.done");

    // 3. SPV (`spv`)
    await apps.locator(".apps-tab", { hasText: "SPV" }).click();
    await apps.getByLabel("SPV secret source").fill("en");
    await apps.getByLabel("SPV observable sink").fill("done");
    await apps.getByRole("button", { name: "Add SPV Rule" }).click();
    await expect(apps).toContainText("spv_1");
    await apps.getByRole("button", { name: "Prove SPV" }).click();

    // 4. SEC / LEC (`sec`)
    await apps.locator(".apps-tab", { hasText: "SEC / LEC" }).click();
    await expect(apps).toContainText("sec_q");
    await apps.getByRole("button", { name: "Prove SEC / LEC" }).click();

    // 5. CSR / Conn (`csr`)
    await apps.locator(".apps-tab", { hasText: "CSR / Conn" }).click();
    await expect(apps).toContainText("CSR assertions: 1");

    // 6. Bus VIP (`protocol`) -> Generate VIP -> Show Protocol Task
    await apps.locator(".apps-tab", { hasText: "Bus VIP" }).click();
    await apps.getByRole("button", { name: "Generate VIP" }).click();
    const showProtoBtn = apps.getByRole("button", { name: /Show Protocol Task/ });
    await expect(showProtoBtn).toBeVisible();
    await showProtoBtn.click();
    await expect(page.getByRole("button", { name: "all tasks" })).toContainText("Task: Protocol");
  });

  test("ShortcutSheet (`?`), BudgetPopover, and ContextBar pass axe-core in light & dark themes and fit 4 viewports cleanly", async ({
    page,
  }) => {
    await provedSession(page);

    // Open Trace so ContextBar is mounted, then open BudgetPopover and test axe in Light theme
    await row(page, "a_never9").dblclick();
    await expect(page.getByRole("region", { name: "Active property context" })).toBeVisible();
    await page.getByRole("button", { name: "Proof budget" }).click();
    await expect(page.getByRole("dialog", { name: "Proof budget settings" })).toBeVisible();

    const lightPopAxe = await new AxeBuilder({ page })
      .include(".context-bar")
      .include(".budget-popover")
      .analyze();
    expect(lightPopAxe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog", { name: "Proof budget settings" })).toHaveCount(0);

    // Open `?` ShortcutSheet via keyboard `?` and scan in Light & Dark themes
    await page.keyboard.press("?");
    const sheet = page.getByRole("dialog", { name: "Keyboard & Formal Workflow Guide" });
    await expect(sheet).toBeVisible();
    const lightSheetAxe = await new AxeBuilder({ page }).include(".shortcut-modal").analyze();
    expect(lightSheetAxe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);

    // Switch to Dark theme and re-verify ShortcutSheet + ContextBar contrast
    await page.getByRole("menuitem", { name: "View", exact: true }).click();
    await page.getByRole("menuitem", { name: "Theme: Dark" }).click();
    await expect(page.locator(":root")).toHaveAttribute("data-theme", "dark");
    await page.getByRole("button", { name: "Keyboard and workflow guide" }).click();
    await expect(sheet).toBeVisible();
    const darkAxe = await new AxeBuilder({ page })
      .include(".shortcut-modal")
      .include(".context-bar")
      .analyze();
    expect(darkAxe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
    await page.keyboard.press("Escape");

    // Sweep 4 viewports (desktop 1440x900, laptop 1000x700, tablet 760x600, Antigravity side-pane 420x700)
    for (const vp of [
      { width: 1440, height: 900 },
      { width: 1000, height: 700 },
      { width: 760, height: 600 },
      { width: 420, height: 700 },
    ]) {
      await page.setViewportSize(vp);
      const geom = await page.evaluate(() => ({
        scrollW: document.documentElement.scrollWidth,
        clientW: document.documentElement.clientWidth,
        scrollH: document.documentElement.scrollHeight,
        clientH: document.documentElement.clientHeight,
      }));
      expect(geom.scrollW, `horizontal overflow at ${vp.width}x${vp.height}`).toBeLessThanOrEqual(geom.clientW);
      expect(geom.scrollH, `vertical overflow at ${vp.width}x${vp.height}`).toBeLessThanOrEqual(geom.clientH);
    }
  });
});
