// Item 7 — Full-Document Keyboard-First & Focus-Restoration Contracts.
//
// Runs on BOTH 1x Chromium (`e2e`) AND 2x Retina WebKit (`webkit-hidpi`), proving:
// 1. Strict Tab / Shift+Tab focus trapping inside `aria-modal="true"` dialogs
//    (CommandPalette & ShortcutSheet) with Escape focus restoration.
// 2. WAI-ARIA Menubar keyboard nav + Escape focus return, TraceView AddSignal
//    popover focus return, and narrow (< 720px) sidebar drawer Escape return.
// 3. A complete zero-mouse keyboard-only formal debug journey (F5 -> filter ->
//    table arrows -> Enter trace -> waveform scrub -> F3 source -> Alt+Left back ->
//    Ctrl+L console -> Ctrl+1..4 dock -> Ctrl+K schematic -> arrow-key splitters)
//    where focus never strands on document.body across pivots.
// 4. Full-document (unscoped) axe-core audits across secondary views (Trace,
//    Schematic, CEX Debug Dockview) and open dialogs in Light & Dark themes.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

async function proveViaKeyboard(page: Page) {
  await page.goto("/?fast=1");
  await expect(page.locator(".table-wrap table.grid tbody tr:not(.empty-row)")).toHaveCount(6);
  await page.keyboard.press("F5");
  await expect(page.locator(".strip-summary")).toContainText("1 cex");
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}

async function switchThemeViaKeyboard(page: Page, label: "Theme: Dark" | "Theme: Light") {
  await page.keyboard.press("Control+k");
  const dlg = page.getByRole("dialog", { name: "Command palette" });
  await expect(dlg).toBeVisible();
  await dlg.getByRole("combobox").fill(label);
  await dlg.press("Enter");
  await expect(dlg).toHaveCount(0);
}

async function selectPropertyByKeyboard(page: Page, propName: string) {
  const wrap = page.locator(".table-wrap");
  await wrap.focus();
  await expect(wrap).toBeFocused();
  await page.keyboard.press("Home");
  for (let i = 0; i < 12; i++) {
    const selText = await wrap.locator("tbody tr.sel").textContent();
    if (selText?.includes(propName)) return;
    await page.keyboard.press("ArrowDown");
  }
  throw new Error(`Could not reach property ${propName} via keyboard ArrowDown`);
}

async function expectZeroSeriousAxe(page: Page, contextLabel: string) {
  const result = await new AxeBuilder({ page }).analyze();
  const severe = result.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => ({
      id: v.id,
      impact: v.impact,
      help: v.help,
      nodes: v.nodes.slice(0, 3).map((n) => n.target.join(" ")),
    }));
  expect(severe, `Full-document axe-core severe violations in ${contextLabel}`).toEqual([]);
}

test.describe("Full-Document Keyboard-First & Focus-Restoration Contracts", () => {
  test("1. aria-modal dialogs (CommandPalette & ShortcutSheet) trap Tab / Shift+Tab and restore focus on Escape", async ({
    page,
  }) => {
    await proveViaKeyboard(page);

    // CommandPalette: focus `#prop-filter` -> Ctrl+K -> Tab & Shift+Tab stay trapped inside modal
    const filter = page.locator("#prop-filter");
    await filter.focus();
    await expect(filter).toBeFocused();

    await page.keyboard.press("Control+k");
    const palette = page.getByRole("dialog", { name: "Command palette" });
    await expect(palette).toBeVisible();
    const paletteInput = palette.getByRole("combobox");
    await expect(paletteInput).toBeFocused();

    for (const key of ["Tab", "Tab", "Tab", "Shift+Tab", "Shift+Tab", "Shift+Tab"]) {
      await page.keyboard.press(key);
      const inside = await page.evaluate(() => {
        const dlg = document.querySelector('[role="dialog"][aria-label="Command palette"]');
        return Boolean(dlg && document.activeElement && dlg.contains(document.activeElement));
      });
      expect(inside, `Focus escaped CommandPalette after ${key}`).toBe(true);
    }

    await page.keyboard.press("Escape");
    await expect(palette).toHaveCount(0);
    await expect(filter).toBeFocused();

    // ShortcutSheet: focus `Prove all` -> `?` -> Tab & Shift+Tab stay trapped inside modal
    const proveBtn = page.getByRole("button", { name: "Prove all" });
    await proveBtn.focus();
    await expect(proveBtn).toBeFocused();

    await page.keyboard.press("?");
    const sheet = page.getByRole("dialog", { name: /Keyboard & Formal Workflow Guide/i });
    await expect(sheet).toBeVisible();
    await expect(sheet.getByRole("button", { name: "Close keyboard and workflow guide" })).toBeFocused();

    for (const key of ["Tab", "Tab", "Shift+Tab", "Shift+Tab"]) {
      await page.keyboard.press(key);
      const inside = await page.evaluate(() => {
        const dlg = document.querySelector(".shortcut-modal");
        return Boolean(dlg && document.activeElement && dlg.contains(document.activeElement));
      });
      expect(inside, `Focus escaped ShortcutSheet after ${key}`).toBe(true);
    }

    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);
    await expect(proveBtn).toBeFocused();
  });

  test("2. MenuBar WAI-ARIA nav, TraceView AddSignal dialog, and narrow sidebar drawer restore focus on Escape", async ({
    page,
  }) => {
    await proveViaKeyboard(page);

    // 2a. MenuBar keyboard navigation & Escape focus return to active menu trigger
    const fileTitle = page.getByRole("menubar", { name: "Main menu" }).getByRole("menuitem", { name: "File" });
    const editTitle = page.getByRole("menubar", { name: "Main menu" }).getByRole("menuitem", { name: "Edit" });
    await fileTitle.focus();
    await expect(fileTitle).toBeFocused();

    await page.keyboard.press("ArrowDown");
    const fileMenu = page.getByRole("menu", { name: "File" });
    await expect(fileMenu).toBeVisible();
    await expect(fileMenu.getByRole("menuitem", { name: /Source script/ })).toBeFocused();

    await page.keyboard.press("ArrowDown");
    await expect(fileMenu.getByRole("menuitem", { name: /Edit script path/ })).toBeFocused();

    await page.keyboard.press("ArrowRight");
    const editMenu = page.getByRole("menu", { name: "Edit" });
    await expect(editMenu).toBeVisible();
    await expect(editMenu.getByRole("menuitem", { name: /Find property/ })).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(editMenu).toHaveCount(0);
    await expect(editTitle).toBeFocused();

    // 2b. TraceView AddSignal popover: keyboard open, arrow into listbox, Escape restores focus to Add signal button
    await selectPropertyByKeyboard(page, "a_never9");
    await page.keyboard.press("Enter");
    await expect(page.locator(".trace")).toBeVisible();

    const addBtn = page.getByRole("button", { name: "Add signal…" });
    await addBtn.focus();
    await page.keyboard.press("Enter");
    const addDialog = page.getByRole("dialog", { name: "Add signal" });
    await expect(addDialog).toBeVisible();
    await expect(addDialog.getByLabel("Filter signals")).toBeFocused();

    await page.keyboard.press("ArrowDown");
    await expect(addDialog.getByRole("option").first()).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(addDialog).toHaveCount(0);
    await expect(addBtn).toBeFocused();

    // 2c. Narrow (< 720px) sidebar drawer: keyboard toggle + Escape closes and restores focus to toggle button
    await page.setViewportSize({ width: 640, height: 800 });
    const drawerToggle = page.getByRole("button", { name: "Toggle tasks sidebar" });
    await expect(drawerToggle).toBeVisible();
    await drawerToggle.focus();
    await page.keyboard.press("Enter");
    await expect(page.locator("aside.left.drawer")).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(page.locator("aside.left.drawer")).toHaveCount(0);
    await expect(drawerToggle).toBeFocused();
  });

  test("3. Complete zero-mouse formal debug journey & keyboard splitter resize without stranding focus on document.body", async ({
    page,
  }) => {
    // Strictly zero mouse clicks or pointer calls in this test!
    await proveViaKeyboard(page);

    // Filter with Ctrl+F, type query, clear with Escape
    await page.keyboard.press("Control+f");
    const filter = page.locator("#prop-filter");
    await expect(filter).toBeFocused();
    await page.keyboard.type("a_never9");
    await expect(page.locator(".table-wrap table.grid tbody tr:not(.empty-row)")).toHaveCount(1);
    await page.keyboard.press("Escape");
    await expect(page.locator(".table-wrap table.grid tbody tr:not(.empty-row)")).toHaveCount(6);

    // Navigate PropTable with keyboard arrows and open CEX trace via Enter
    await selectPropertyByKeyboard(page, "a_never9");
    await page.keyboard.press("Enter");
    const trace = page.locator(".trace");
    await expect(trace).toBeVisible();
    // Focus lands automatically on `.trace` (not stranded on document.body!)
    await expect(trace).toBeFocused();

    // Scrub waveform purely with keyboard: Home -> ArrowRight -> ArrowLeft -> N -> P -> End
    const cursorStatus = page.getByRole("status", { name: "Cursor" });
    await expect(cursorStatus).toContainText("cycle 11");
    await page.keyboard.press("Home");
    await expect(cursorStatus).toContainText("cycle 0");
    await page.keyboard.press("ArrowRight");
    await expect(cursorStatus).toContainText("cycle 1");
    await page.keyboard.press("ArrowLeft");
    await expect(cursorStatus).toContainText("cycle 0");
    await page.keyboard.press("n");
    const afterNext = (await cursorStatus.textContent()) ?? "";
    expect(afterNext).not.toContain("cycle 0");
    await page.keyboard.press("p");
    await page.keyboard.press("End");
    await expect(cursorStatus).toContainText("cycle 11");

    // Pivot to RTL source with F3, then Alt+Left back to Trace, then Alt+Left back to Table
    await page.keyboard.press("F3");
    await expect(page.locator(".center .cm-editor")).toBeVisible();

    await page.keyboard.press("Alt+ArrowLeft");
    await expect(trace).toBeVisible();
    await expect(trace).toBeFocused();

    await page.keyboard.press("Alt+ArrowLeft");
    const tableWrap = page.locator(".table-wrap");
    await expect(tableWrap).toBeVisible();
    await expect(tableWrap).toBeFocused();

    // Console focus via Ctrl+L, execute `status`, switch bottom dock tabs via Ctrl+2..4 & Ctrl+1
    await page.keyboard.press("Control+l");
    const consoleInput = page.locator("#console-input");
    await expect(consoleInput).toBeFocused();
    await page.keyboard.type("status");
    await page.keyboard.press("Enter");
    await expect(page.locator(".console-log")).toContainText("status");

    await page.keyboard.press("Control+2");
    await expect(page.locator(".bottom .tab.on")).toContainText("Details");
    await page.keyboard.press("Control+3");
    await expect(page.locator(".bottom .tab.on")).toContainText("Messages");
    await page.keyboard.press("Control+4");
    await expect(page.locator(".bottom .tab.on")).toContainText("Log");
    await page.keyboard.press("Control+1");
    await expect(page.locator(".bottom .tab.on")).toContainText("Console");

    // Open COI Schematic via Command Palette (`Ctrl+K`), then `Alt+ArrowLeft` back to Table
    await page.keyboard.press("Control+k");
    const palette = page.getByRole("dialog", { name: "Command palette" });
    await expect(palette).toBeVisible();
    await palette.getByRole("combobox").fill("Show COI");
    await page.keyboard.press("Enter");
    await expect(page.locator(".schematic-pane canvas")).toBeVisible();

    await page.keyboard.press("Alt+ArrowLeft");
    await expect(tableWrap).toBeVisible();
    await expect(tableWrap).toBeFocused();

    // Keyboard resize both splitters via `[role="separator"]` + verify localStorage persistence
    const leftSep = page.getByRole("separator", { name: "Resize left dock" });
    const bottomSep = page.getByRole("separator", { name: "Resize bottom dock" });
    const leftBefore = Number(await leftSep.getAttribute("aria-valuenow"));
    const bottomBefore = Number(await bottomSep.getAttribute("aria-valuenow"));

    await leftSep.focus();
    await expect(leftSep).toBeFocused();
    for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowRight");
    const leftAfter = Number(await leftSep.getAttribute("aria-valuenow"));
    expect(leftAfter).toBeGreaterThanOrEqual(leftBefore + 48);

    await bottomSep.focus();
    await expect(bottomSep).toBeFocused();
    for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowUp");
    const bottomAfter = Number(await bottomSep.getAttribute("aria-valuenow"));
    expect(bottomAfter).toBeGreaterThanOrEqual(bottomBefore + 48);

    const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("informal-layout") || "{}"));
    expect(saved.leftW).toBeGreaterThanOrEqual(leftBefore + 48);
    expect(saved.bottomH).toBeGreaterThanOrEqual(bottomBefore + 48);
  });

  test("4. Full-document axe-core passes across Trace, Schematic, CEX Debug Dockview, and open dialogs in Light & Dark", async ({
    page,
  }) => {
    test.setTimeout(90_000);
    await proveViaKeyboard(page);

    for (const theme of ["light", "dark"] as const) {
      if (theme === "dark") {
        await switchThemeViaKeyboard(page, "Theme: Dark");
        await expect(page.locator(":root")).toHaveAttribute("data-theme", "dark");
      }

      // 4a. Open Trace view + AddSignal popover
      await selectPropertyByKeyboard(page, "a_never9");
      await page.keyboard.press("Enter");
      await expect(page.locator(".trace canvas.trace-canvas")).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: Trace view`);

      await page.getByRole("button", { name: "Add signal…" }).click();
      await expect(page.getByRole("dialog", { name: "Add signal" })).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: Add signal dialog`);
      await page.keyboard.press("Escape");

      // 4b. Open Schematic view
      await page.getByRole("tab", { name: "Schematic" }).click();
      await expect(page.locator(".schematic-pane canvas")).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: Schematic view`);

      // 4c. Switch to CEX Debug Dockview layout
      await page.getByRole("combobox", { name: "Layout" }).selectOption("cex-debug");
      await expect(page.getByTestId("dockview-workspace")).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: CEX Debug Dockview`);

      // Return to Triage for modal/popover full-document audits
      await page.getByRole("combobox", { name: "Layout" }).selectOption("triage");
      await page.getByRole("tab", { name: "Property Table" }).click();
      await expect(page.locator(".table-wrap")).toBeVisible();

      // 4d. Open CommandPalette
      await page.keyboard.press("Control+k");
      await expect(page.getByRole("dialog", { name: "Command palette" })).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: Command palette modal`);
      await page.keyboard.press("Escape");

      // 4e. Open ShortcutSheet
      await page.getByRole("button", { name: "Prove all" }).focus();
      await page.keyboard.press("?");
      await expect(page.getByRole("dialog", { name: /Keyboard & Formal Workflow Guide/i })).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: Shortcut sheet modal`);
      await page.keyboard.press("Escape");

      // 4f. Open Proof budget popover
      await page.getByRole("button", { name: "Proof budget" }).click();
      await expect(page.getByRole("dialog", { name: "Proof budget settings" })).toBeVisible();
      await expectZeroSeriousAxe(page, `${theme}: Proof budget popover`);
      await page.keyboard.press("Escape");
    }
  });
});
