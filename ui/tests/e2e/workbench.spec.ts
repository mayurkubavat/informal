import { test, expect, Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

// Drives the workbench against the browser mock in src/backend.ts: six
// properties on counter.sv; `prove`/`check_fv` resolve them to 2 proven,
// 1 cex, 1 covered, 1 unreachable; any other Tcl returns an error line.

const rows = (page: Page) => page.locator("table.grid tbody tr:not(.empty-row)");
const row = (page: Page, name: string) => rows(page).filter({ has: page.getByRole("cell", { name, exact: true }) });
const summary = (page: Page) => page.locator(".strip-summary");
const statusbar = (page: Page) => page.locator("footer.statusbar");
const treeGroup = (page: Page, label: string) => page.locator(".tree-row.child").filter({ hasText: new RegExp(`^${label}\\d+$`) });

async function proveAll(page: Page) {
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(statusbar(page)).toContainText("Idle");
  await expect(summary(page)).toContainText("2 proven");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(rows(page)).toHaveCount(6);
});

test("initial load shows six properties and the counter.sv tab", async ({ page }) => {
  await expect(page.getByRole("tab", { name: "counter.sv" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "Property Table" })).toHaveAttribute("aria-selected", "true");
  await expect(summary(page)).toHaveText("6 properties · 5 unprocessed · 1 assumed");
  for (const n of ["a_max", "a_step", "a_never9", "c_done", "c_10", "m_en"]) await expect(row(page, n)).toHaveCount(1);
  await expect(statusbar(page)).toContainText("top: counter");
  await expect(statusbar(page)).toContainText("JasperGold flavor");
  await expect(page.locator(".prompt")).toHaveText("fdv[jg]>");
});

test("Prove all updates tree, strip and status bar", async ({ page }) => {
  await proveAll(page);
  await expect(summary(page)).toHaveText("6 properties · 2 proven · 1 cex · 1 covered · 1 unreachable · 1 assumed");
  await expect(treeGroup(page, "Proven")).toContainText("2");
  await expect(treeGroup(page, "CEX")).toContainText("1");
  await expect(treeGroup(page, "Covered")).toContainText("1");
  await expect(treeGroup(page, "Unreachable")).toContainText("1");
  await expect(treeGroup(page, "Assumed")).toContainText("1");
  await expect(page.locator(".tree-row.child").filter({ hasText: "Unprocessed" })).toHaveCount(0);
  await expect(row(page, "a_never9")).toContainText("CEX");
  await expect(row(page, "a_never9")).toContainText("11");
  await expect(row(page, "a_max")).toContainText("∞");
  await expect(statusbar(page)).toContainText("Idle");
  await expect(page.locator(".console-log")).toContainText("fdv[jg]> prove -all");
});

test("CEX tree group filters to a_never9; clear status filter restores", async ({ page }) => {
  await proveAll(page);
  await treeGroup(page, "CEX").click();
  await expect(rows(page)).toHaveCount(1);
  await expect(row(page, "a_never9")).toHaveCount(1);
  await page.getByRole("button", { name: "clear status filter" }).click();
  await expect(rows(page)).toHaveCount(6);
  await expect(page.getByRole("button", { name: "clear status filter" })).toHaveCount(0);
});

test("selecting a row opens Details with its expression", async ({ page }) => {
  await row(page, "a_step").click();
  await expect(page.getByRole("tab", { name: "Details: a_step" })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".details pre.expr")).toHaveText("en && !done |=> q == $past(q) + 4'd1");
  await expect(row(page, "a_step")).toHaveAttribute("aria-selected", "true");
});

test("flavor change switches prompt and Prove menu hint", async ({ page }) => {
  await page.getByLabel("Flavor").selectOption("vcf");
  await expect(page.locator(".prompt")).toHaveText("fdv[vcf]>");
  await expect(statusbar(page)).toContainText("VC Formal flavor");
  await page.getByRole("menuitem", { name: "Prove", exact: true }).click();
  await expect(page.locator(".menu-pop .menu-hint")).toHaveText("check_fv");
});

test("console echoes a command and shows the mock's error line", async ({ page }) => {
  const input = page.getByLabel("Tcl command");
  await input.fill("get_design_info");
  await input.press("Enter");
  await expect(page.locator(".console-log .line.in").last()).toHaveText("fdv[jg]> get_design_info");
  await expect(page.locator(".console-log .line.err").last()).toHaveText("browser preview: Tcl runs only in the desktop app");
  await expect(input).toHaveValue("");
  await expect(page.getByRole("tab", { name: "Messages (1)" })).toBeVisible();
});

test("sorting by Name cycles aria-sort", async ({ page }) => {
  const th = page.getByRole("columnheader", { name: "Name" });
  await expect(th).not.toHaveAttribute("aria-sort", /.*/);
  await th.click();
  await expect(th).toHaveAttribute("aria-sort", "ascending");
  await expect(rows(page).first()).toContainText("a_max");
  await th.click();
  await expect(th).toHaveAttribute("aria-sort", "descending");
  await expect(rows(page).first()).toContainText("m_en");
  await th.click();
  await expect(th).not.toHaveAttribute("aria-sort", /.*/);
  await expect(rows(page).first()).toContainText("a_max");
});

test("Up/Down moves the selection", async ({ page }) => {
  await row(page, "a_max").click();
  const wrap = page.locator(".table-wrap");
  await wrap.press("ArrowDown");
  await expect(row(page, "a_step")).toHaveAttribute("aria-selected", "true");
  await wrap.press("ArrowDown");
  await expect(row(page, "a_never9")).toHaveAttribute("aria-selected", "true");
  await wrap.press("ArrowUp");
  await expect(row(page, "a_step")).toHaveAttribute("aria-selected", "true");
  await expect(row(page, "a_max")).toHaveAttribute("aria-selected", "false");
});

test("View > Theme: Dark sets data-theme", async ({ page }) => {
  await page.getByRole("menuitem", { name: "View", exact: true }).click();
  await page.getByRole("menuitem", { name: "Theme: Dark" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator(".menu-pop")).toHaveCount(0);
});

test("no serious or critical axe violations", async ({ page }) => {
  await proveAll(page);
  const r = await new AxeBuilder({ page }).analyze();
  const bad = r.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
  expect(bad.map((v) => `${v.id} (${v.impact}): ${v.help} @ ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
});

test.describe("task tree properties", () => {
  const leaf = (page: Page, name: string) => page.locator(".tree-row.leaf").filter({ hasText: new RegExp(`${name}$`) });

  test("lists properties under their status groups", async ({ page }) => {
    await expect(page.locator(".tree-row.leaf")).toHaveCount(6);
    await proveAll(page);
    const tree = page.getByRole("tree", { name: "Tasks" });
    await expect(tree.getByRole("treeitem")).toHaveText([
      /<embedded>6/, /Proven2/, /a_max/, /a_step/, /CEX1/, /a_never9/, /Covered1/, /c_done/, /Unreachable1/, /c_10/, /Assumed1/, /m_en/,
    ]);
  });

  test("click selects the table row; double-click opens Details", async ({ page }) => {
    await page.getByRole("tab", { name: "Console" }).click();
    await leaf(page, "a_never9").click();
    await expect(row(page, "a_never9")).toHaveAttribute("aria-selected", "true");
    await expect(leaf(page, "a_never9")).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("tab", { name: "Console" })).toHaveAttribute("aria-selected", "true");
    await leaf(page, "c_done").dblclick();
    await expect(page.getByRole("tab", { name: "Details: c_done" })).toHaveAttribute("aria-selected", "true");
    await expect(page.locator(".details pre.expr")).toHaveText("$rose(done)");
  });
});

test.describe("row context menu", () => {
  test.use({ permissions: ["clipboard-read", "clipboard-write"] });
  const menu = (page: Page) => page.getByRole("menu", { name: /^Actions for/ });
  const item = (page: Page, name: string | RegExp) => menu(page).getByRole("menuitem", { name });

  test("offers the row actions and selects the row", async ({ page }) => {
    await row(page, "a_never9").click({ button: "right" });
    await expect(menu(page)).toHaveAccessibleName("Actions for a_never9");
    await expect(menu(page).getByRole("menuitem")).toHaveText([
      /Prove\s*prove -property a_never9/, "Visualize trace", "Show source", "Copy name", "Copy expression", /Disable/,
    ]);
    await expect(row(page, "a_never9")).toHaveAttribute("aria-selected", "true");
    await expect(item(page, "Visualize trace")).toHaveAttribute("aria-disabled", "true");
    await expect(item(page, /Disable/)).toHaveAttribute("aria-disabled", "true");
    await expect(item(page, /Disable/)).toHaveAttribute("title", /no disable command/);
    await page.keyboard.press("Escape");
    await expect(menu(page)).toHaveCount(0);
  });

  test("Prove runs only that property", async ({ page }) => {
    await row(page, "c_done").click({ button: "right" });
    await item(page, /^Prove/).click();
    await expect(row(page, "c_done")).toContainText("Covered");
    await expect(row(page, "a_max")).toContainText("Unprocessed");
    await expect(page.locator(".console-log")).toContainText("fdv[jg]> prove -property c_done");
  });

  test("Show source opens the source tab at the property's line", async ({ page }) => {
    await row(page, "a_never9").click({ button: "right" });
    await item(page, "Show source").click();
    await expect(page.getByRole("tab", { name: "counter.sv" })).toHaveAttribute("aria-selected", "true");
    await expect(page.locator("main pre.source .focus")).toContainText("output logic [3:0] q");
  });

  test("Copy name and Copy expression use the clipboard", async ({ page }) => {
    await row(page, "a_step").click({ button: "right" });
    await item(page, "Copy name").click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe("a_step");
    await row(page, "a_step").click({ button: "right" });
    await item(page, "Copy expression").click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe("en && !done |=> q == $past(q) + 4'd1");
  });

  test("VC Formal disables and re-enables via fvdisable/fvenable", async ({ page }) => {
    await page.getByLabel("Flavor").selectOption("vcf");
    await expect(page.locator(".prompt")).toHaveText("fdv[vcf]>");
    await row(page, "a_max").click({ button: "right" });
    await expect(item(page, /Disable/)).toContainText("fvdisable {a_max}");
    await item(page, /Disable/).click();
    await expect(row(page, "a_max")).toContainText("Disabled");
    await expect(row(page, "a_max")).toHaveClass(/off/);
    await row(page, "a_max").click({ button: "right" });
    await item(page, /Enable/).click();
    await expect(row(page, "a_max")).toContainText("Unprocessed");
    await expect(page.locator(".console-log")).toContainText("fdv[vcf]> fvenable {a_max}");
  });

  test("Shift+F10 opens it from the keyboard; arrows move; focus returns", async ({ page }) => {
    await row(page, "a_max").click();
    const wrap = page.locator(".table-wrap");
    await wrap.press("Shift+F10");
    await expect(menu(page)).toHaveAccessibleName("Actions for a_max");
    await expect(item(page, /^Prove/)).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(item(page, "Visualize trace")).toBeFocused();
    await page.keyboard.press("End");
    await expect(item(page, /Disable/)).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(menu(page)).toHaveCount(0);
    await expect(wrap).toBeFocused();
  });

  test("an outside click closes it", async ({ page }) => {
    await row(page, "a_max").click({ button: "right" });
    await page.locator("footer.statusbar").click();
    await expect(menu(page)).toHaveCount(0);
  });
});

test.describe("keyboard model", () => {
  test("F5 proves all, F6 proves the selected property", async ({ page }) => {
    await row(page, "c_10").click();
    await page.keyboard.press("F6");
    await expect(row(page, "c_10")).toContainText("Unreachable");
    await expect(row(page, "a_max")).toContainText("Unprocessed");
    await page.keyboard.press("F5");
    await expect(summary(page)).toHaveText("6 properties · 2 proven · 1 cex · 1 covered · 1 unreachable · 1 assumed");
  });

  test("Enter opens Details for the selected row", async ({ page }) => {
    await row(page, "a_max").click();
    await page.getByRole("tab", { name: "Console" }).click();
    const wrap = page.locator(".table-wrap");
    await wrap.focus();
    await wrap.press("ArrowDown");
    await wrap.press("Enter");
    await expect(page.getByRole("tab", { name: "Details: a_step" })).toHaveAttribute("aria-selected", "true");
  });

  test("F3 toggles the source view", async ({ page }) => {
    await page.keyboard.press("F3");
    await expect(page.getByRole("tab", { name: "counter.sv" })).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("F3");
    await expect(page.getByRole("tab", { name: "Property Table" })).toHaveAttribute("aria-selected", "true");
  });

  test("Ctrl+F focuses the filter, even from the source view", async ({ page }) => {
    await page.keyboard.press("F3");
    await page.keyboard.press("Control+f");
    await expect(page.getByLabel("Filter properties")).toBeFocused();
    await page.keyboard.type("never");
    await expect(rows(page)).toHaveCount(1);
  });

  test("Ctrl+L jumps to the console prompt from another dock tab", async ({ page }) => {
    await page.getByRole("tab", { name: "Messages" }).click();
    await page.keyboard.press("Control+l");
    await expect(page.getByLabel("Tcl command")).toBeFocused();
    await expect(page.getByRole("tab", { name: "Console" })).toHaveAttribute("aria-selected", "true");
  });

  test("Ctrl+1..3 switch the bottom dock tabs", async ({ page }) => {
    const tab = (n: RegExp) => page.getByRole("tab", { name: n });
    await page.keyboard.press("Control+3");
    await expect(tab(/^Messages/)).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("Control+2");
    await expect(tab(/^Details/)).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("Control+1");
    await expect(tab(/^Console/)).toHaveAttribute("aria-selected", "true");
  });

  test("Esc closes an open menu; Stop is listed but disabled", async ({ page }) => {
    await page.getByRole("menuitem", { name: "Prove", exact: true }).click();
    const stop = page.locator(".menu-pop").getByRole("menuitem", { name: /Stop/ });
    await expect(stop).toBeDisabled();
    await expect(stop).toContainText("Esc");
    await expect(page.locator(".menu-pop").getByRole("menuitem", { name: /Prove all/ })).toContainText("F5");
    await page.keyboard.press("Escape");
    await expect(page.locator(".menu-pop")).toHaveCount(0);
  });

  test("Help > Keyboard shortcuts prints the list in the console", async ({ page }) => {
    await page.getByRole("menuitem", { name: "Help", exact: true }).click();
    await page.getByRole("menuitem", { name: "Keyboard shortcuts" }).click();
    await expect(page.locator(".console-log")).toContainText("Ctrl+1..3");
    await expect(page.locator(".console-log")).toContainText("Prove selected");
  });
});

test.describe("filter", () => {
  const box = (page: Page) => page.getByLabel("Filter properties");
  const count = (page: Page) => page.locator(".filter-count");

  test("is case-insensitive across name, expression and engine, with a match count", async ({ page }) => {
    await expect(count(page)).toHaveCount(0);
    await box(page).fill("A_MAX");
    await expect(rows(page)).toHaveCount(1);
    await expect(count(page)).toHaveText("1 of 6");
    await box(page).fill("Q != 4'D9");
    await expect(rows(page)).toHaveCount(1);
    await expect(row(page, "a_never9")).toHaveCount(1);
    await proveAll(page);
    await box(page).fill("BMC");
    await expect(rows(page)).toHaveCount(2);
    await expect(count(page)).toHaveText("2 of 6");
    await box(page).fill("zzz");
    await expect(page.getByText("No properties match the current filter.")).toBeVisible();
    await expect(count(page)).toHaveText("0 of 6");
  });

  test("combines with the status filter and Esc clears the text", async ({ page }) => {
    await proveAll(page);
    await treeGroup(page, "Proven").click();
    await expect(count(page)).toHaveText("2 of 6");
    await box(page).fill("step");
    await expect(count(page)).toHaveText("1 of 6");
    await box(page).press("Escape");
    await expect(box(page)).toHaveValue("");
    await expect(count(page)).toHaveText("2 of 6");
  });
});
