import { test, expect, Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

// Drives the workbench against the browser mock in src/backend.ts: six
// properties on counter.sv; `prove`/`check_fv` resolve them to 2 proven,
// 1 cex, 1 covered, 1 unreachable; any other Tcl returns an error line.
// Most tests load /?fast=1 so a prove finishes in one step; the "live
// progress" block runs the ticking mock.

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
  await page.goto("/?fast=1");
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

test.describe("layout", () => {
  const leftSplit = (page: Page) => page.getByRole("separator", { name: "Resize left dock" });
  const leftWidth = (page: Page) => page.locator("aside.left").evaluate((e) => Math.round(e.getBoundingClientRect().width));
  const bottomHeight = (page: Page) => page.locator("section.bottom").evaluate((e) => Math.round(e.getBoundingClientRect().height));
  async function dragBy(page: Page, name: string, dx: number, dy: number) {
    const box = (await page.getByRole("separator", { name }).boundingBox())!;
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + dx / 2, y + dy / 2);
    await page.mouse.move(x + dx, y + dy);
    await page.mouse.up();
  }

  test("left dock resizes by drag within 160-480px", async ({ page }) => {
    expect(await leftWidth(page)).toBe(236);
    await dragBy(page, "Resize left dock", 100, 0);
    await expect.poll(() => leftWidth(page)).toBe(336);
    await dragBy(page, "Resize left dock", 1000, 0);
    await expect.poll(() => leftWidth(page)).toBe(480);
    await dragBy(page, "Resize left dock", -1000, 0);
    await expect.poll(() => leftWidth(page)).toBe(160);
    await expect(leftSplit(page)).toHaveAttribute("aria-valuenow", "160");
  });

  test("splitters work from the keyboard and reset on double-click", async ({ page }) => {
    await leftSplit(page).focus();
    await page.keyboard.press("ArrowRight");
    await expect.poll(() => leftWidth(page)).toBe(252);
    await leftSplit(page).dblclick();
    await expect.poll(() => leftWidth(page)).toBe(236);
    const bottom = page.getByRole("separator", { name: "Resize bottom dock" });
    const h0 = await bottomHeight(page);
    await bottom.focus();
    await page.keyboard.press("ArrowUp");
    await expect.poll(() => bottomHeight(page)).toBe(h0 + 16);
  });

  test("sizes and open tabs persist across reloads in fdv-layout", async ({ page }) => {
    await dragBy(page, "Resize left dock", 64, 0);
    await dragBy(page, "Resize bottom dock", 0, -100);
    await page.getByRole("tab", { name: "Design" }).click();
    await page.getByRole("tab", { name: "counter.sv" }).click();
    await page.getByRole("tab", { name: "Messages" }).click();
    const h = await bottomHeight(page);
    await expect.poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("fdv-layout") ?? "{}"))).toMatchObject({
      leftW: 300, left: "design", center: "source", bottom: "messages", leftOpen: true,
    });
    await page.reload();
    await expect.poll(() => leftWidth(page)).toBe(300);
    await expect.poll(() => bottomHeight(page)).toBe(h);
    await expect(page.getByRole("tab", { name: "Design" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("tab", { name: "counter.sv" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("tab", { name: "Messages" })).toHaveAttribute("aria-selected", "true");
  });

  test("a corrupt fdv-layout falls back to the default layout", async ({ page }) => {
    await page.evaluate(() => localStorage.setItem("fdv-layout", "{oops"));
    await page.reload();
    await expect(rows(page)).toHaveCount(6);
    expect(await leftWidth(page)).toBe(236);
    await expect(page.getByRole("tab", { name: "Tasks" })).toHaveAttribute("aria-selected", "true");
  });
});

// The ticking mock (src/mock.ts): two properties at a time, 300ms ticks
// unless ?tick is given; ?freeze=2 holds the run with a_max proven,
// a_step and a_never9 running and c_done, c_10 queued; ?freeze=4 holds it
// with three done and c_done, c_10 running.
test.describe("live progress", () => {
  const proveBtn = (page: Page) => page.getByRole("button", { name: "Prove all" });
  const stopBtn = (page: Page) => page.getByRole("button", { name: /^Stop/ });
  const bar = (page: Page) => page.getByRole("progressbar", { name: "Prove progress" });
  async function start(page: Page, query: string) {
    await page.goto(`/?${query}`);
    await expect(rows(page)).toHaveCount(6);
    await expect(statusbar(page)).toContainText("Idle");
    await expect(stopBtn(page)).toBeDisabled();
    await proveBtn(page).click();
  }

  test("prove shows running rows, then the final counts", async ({ page }) => {
    await start(page, "tick=150");
    await expect(row(page, "a_max")).toContainText("Running");
    await expect(row(page, "a_max").locator(".st-running")).toHaveCount(1);
    await expect(row(page, "a_step")).toContainText(/k=\d+/);
    await expect(treeGroup(page, "Running")).toContainText("2");
    await expect(statusbar(page)).toContainText(/Proving · \d\/5/);
    await expect(bar(page)).toHaveAttribute("aria-valuemax", "5");
    await expect(page.locator(".progress-text")).toHaveText(/^\d of 5 · 0:0\d$/);
    await expect(proveBtn(page)).toBeDisabled();
    await expect(stopBtn(page)).toBeEnabled();
    await expect(stopBtn(page)).toHaveText("Stop");
    await expect(stopBtn(page)).toHaveAttribute("title", "Stop the running prove (Esc)");
    await expect(summary(page)).toHaveText("6 properties · 2 proven · 1 cex · 1 covered · 1 unreachable · 1 assumed");
    await expect(statusbar(page)).toContainText("Idle");
    await expect(bar(page)).toHaveCount(0);
    await expect(treeGroup(page, "Running")).toHaveCount(0);
    await expect(proveBtn(page)).toBeEnabled();
    await expect(stopBtn(page)).toBeDisabled();
  });

  test("a frozen run shows the mid-run frame", async ({ page }) => {
    await start(page, "freeze=2");
    await expect(statusbar(page)).toContainText("Proving · 1/5");
    await expect(page.locator(".progress-text")).toHaveText("1 of 5 · 0:00");
    await expect(bar(page)).toHaveAttribute("aria-valuenow", "1");
    await expect(row(page, "a_max")).toContainText("Proven");
    await expect(row(page, "a_step")).toContainText("k=11");
    await expect(row(page, "a_never9")).toContainText("k=1");
    await expect(row(page, "c_10")).toContainText("Unprocessed");
    await expect(summary(page)).toHaveText("6 properties · 2 running · 1 proven · 2 unprocessed · 1 assumed");
    await row(page, "a_step").click();
    await expect(page.locator(".details .details-title")).toContainText("Running");
    await expect(page.getByRole("button", { name: "Re-prove" })).toBeDisabled();
  });

  test("Stop mid-run leaves undetermined (stopped) rows and re-enables Prove all", async ({ page }) => {
    await start(page, "freeze=2");
    await expect(statusbar(page)).toContainText("Proving · 1/5");
    await stopBtn(page).click();
    for (const n of ["a_step", "a_never9", "c_done", "c_10"]) {
      await expect(row(page, n)).toContainText("Undetermined");
      await expect(row(page, n)).toContainText("stopped");
    }
    await expect(row(page, "a_max")).toContainText("Proven");
    await expect(summary(page)).toHaveText("6 properties · 1 proven · 4 undetermined · 1 assumed");
    await expect(page.locator(".console-log .line.err").last()).toHaveText("prove stopped: 4 properties left undetermined");
    await expect(statusbar(page)).toContainText("Idle");
    await expect(proveBtn(page)).toBeEnabled();
    await expect(stopBtn(page)).toBeDisabled();
    await expect(stopBtn(page)).toHaveText(/Stop/);
  });

  test("Stop reads Stopping… until the backend confirms", async ({ page }) => {
    await start(page, "freeze=2&holdStop=1");
    await expect(statusbar(page)).toContainText("Proving · 1/5");
    await stopBtn(page).click();
    await expect(stopBtn(page)).toHaveText("Stopping…");
    await expect(stopBtn(page)).toBeDisabled();
    await expect(statusbar(page)).toContainText("Stopping…");
    await page.getByRole("menuitem", { name: "Prove", exact: true }).click();
    await expect(page.locator(".menu-pop").getByRole("menuitem", { name: /Stopping/ })).toBeDisabled();
  });

  test("Esc stops a running prove, but closes an open menu first", async ({ page }) => {
    await start(page, "freeze=2");
    await expect(statusbar(page)).toContainText("Proving · 1/5");
    await page.getByRole("menuitem", { name: "Prove", exact: true }).click();
    const stopItem = page.locator(".menu-pop").getByRole("menuitem", { name: /Stop/ });
    await expect(stopItem).toBeEnabled();
    await page.keyboard.press("Escape");
    await expect(page.locator(".menu-pop")).toHaveCount(0);
    await expect(statusbar(page)).toContainText("Proving");
    await page.keyboard.press("Escape");
    await expect(row(page, "a_step")).toContainText("stopped");
    await expect(statusbar(page)).toContainText("Idle");
  });

  test("the console gets engine lines live and stays usable; a second prove is refused", async ({ page }) => {
    await start(page, "freeze=4");
    const log = page.locator(".console-log");
    await expect(statusbar(page)).toContainText("Proving · 3/5");
    await expect(log.locator(".line.in").last()).toHaveText("fdv[jg]> prove -all");
    await expect(log.locator(".line.out")).toHaveText([
      "a_max: proven [ric3-ic3, 41 ms]",
      "a_step: proven [ric3, 55 ms]",
      "a_never9: cex at depth 11 [ric3-bmc, 30 ms]",
    ]);
    const input = page.getByLabel("Tcl command");
    await expect(input).toBeEnabled();
    await input.fill("prove -all");
    await input.press("Enter");
    await expect(log.locator(".line.err").last()).toHaveText("a prove is already running; stop it first (Esc)");
    await input.fill("get_design_info");
    await input.press("Enter");
    await expect(log.locator(".line.err").last()).toHaveText("browser preview: Tcl runs only in the desktop app");
    await page.keyboard.press("F5");
    await expect(statusbar(page)).toContainText("Proving · 3/5");
    await expect(log.locator(".line.in").filter({ hasText: "prove -all" })).toHaveCount(2);
  });

  test("at 1000x700 the tool row stays on one line while a prove runs", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 700 });
    await start(page, "freeze=2");
    await expect(statusbar(page)).toContainText("Proving · 1/5");
    await expect(bar(page)).toBeVisible();
    // Under 1000 px only the bar shows; at 1000 px the readout fits too.
    const prove = (await proveBtn(page).boundingBox())!;
    const stop = (await stopBtn(page).boundingBox())!;
    const track = (await bar(page).boundingBox())!;
    for (const b of [prove, stop]) expect(b.height).toBeLessThanOrEqual(28);
    expect(Math.abs(prove.y - stop.y)).toBeLessThan(1);
    expect(stop.x).toBeGreaterThanOrEqual(prove.x + prove.width);
    expect(track.x).toBeGreaterThan(stop.x + stop.width);
    const rowBox = (await page.locator(".toolrow").boundingBox())!;
    expect(track.x + track.width).toBeLessThanOrEqual(rowBox.x + rowBox.width);
    await page.setViewportSize({ width: 960, height: 700 });
    await expect(page.locator(".progress-text")).toBeHidden();
    await expect(bar(page)).toBeVisible();
    // Narrower still, the buttons keep their size and line rather than wrap.
    await page.setViewportSize({ width: 820, height: 700 });
    const prove2 = (await proveBtn(page).boundingBox())!;
    const stop2 = (await stopBtn(page).boundingBox())!;
    for (const b of [prove2, stop2]) expect(b.height).toBeLessThanOrEqual(28);
    expect(Math.abs(prove2.y - stop2.y)).toBeLessThan(1);
    expect(stop2.width).toBeCloseTo(stop.width, 0);
  });
});

test.describe("a script that adds properties and proves them", () => {
  const path = "mock/prove-on-source.tcl";
  async function source(page: Page, query: string) {
    await page.goto(`/?${query}`);
    await expect(rows(page)).toHaveCount(6);
    const input = page.getByRole("textbox", { name: "Script path" });
    await input.fill(path);
    await input.press("Enter");
  }

  test("shows the new properties as Running rows during the run", async ({ page }) => {
    await source(page, "freeze=1");
    await expect(rows(page)).toHaveCount(8);
    for (const [n, letter] of [["a_src_ok", "A"], ["c_src_hit", "C"]]) {
      await expect(row(page, n)).toContainText("Running");
      await expect(row(page, n)).toHaveClass(/\bprov\b/);
      await expect(row(page, n).locator("td").first()).toHaveText(letter);
    }
    await expect(row(page, "c_src_hit")).toContainText("k=6");
    await expect(summary(page)).toContainText("2 running");
    await expect(statusbar(page)).toContainText("Proving · 0/2");
    await expect(page.getByRole("button", { name: /^Stop/ })).toBeEnabled();
    await page.getByRole("button", { name: /^Stop/ }).click();
    await expect(statusbar(page)).toContainText("Idle");
    await expect(rows(page)).toHaveCount(8);
    for (const n of ["a_src_ok", "c_src_hit"]) {
      await expect(row(page, n)).toContainText("stopped");
      await expect(row(page, n)).not.toHaveClass(/\bprov\b/);
    }
  });

  test("the reply's session replaces the provisional rows", async ({ page }) => {
    await source(page, "tick=150");
    await expect(row(page, "a_src_ok")).toContainText("Running");
    await expect(statusbar(page)).toContainText("Idle");
    await expect(rows(page)).toHaveCount(8);
    await expect(row(page, "a_src_ok")).toContainText("Proven");
    await expect(row(page, "c_src_hit")).toContainText("Covered");
    await expect(page.locator("table.grid tr.prov")).toHaveCount(0);
    await row(page, "c_src_hit").click();
    await expect(page.locator(".details")).toContainText("q == 4'd4");
  });
});

test.describe("trace tab", () => {
  const traceTab = (page: Page) => page.getByRole("tab", { name: /^Trace: / });
  const cursor = (page: Page) => page.getByRole("status", { name: "Cursor" });
  const visualize = (page: Page) => page.locator(".toolrow").getByRole("button", { name: /^Visualize trace/ });

  test.beforeEach(async ({ page }) => {
    await proveAll(page);
  });

  test("double-click or Enter on a CEX row opens it; the tab closes", async ({ page }) => {
    await row(page, "a_never9").dblclick();
    await expect(traceTab(page)).toHaveText("Trace: a_never9");
    await expect(traceTab(page)).toHaveAttribute("aria-selected", "true");
    await expect(page.locator(".trace-bar .strip-summary")).toHaveText("CEX · depth 11 · ric3-bmc · 12 cycles");
    await expect(cursor(page)).toHaveText("cycle 11");
    await page.getByRole("button", { name: "Close Trace: a_never9" }).click();
    await expect(traceTab(page)).toHaveCount(0);
    await expect(page.getByRole("tab", { name: "Property Table" })).toHaveAttribute("aria-selected", "true");
    // Enter on the selected covered row
    await row(page, "c_done").click();
    await page.locator(".table-wrap").press("Enter");
    await expect(traceTab(page)).toHaveText("Trace: c_done");
    await expect(page.locator(".trace-bar .strip-summary")).toHaveText("Covered · depth 11 · ric3-bmc · 12 cycles");
  });

  test("Enter on a proven row still opens details, not a trace", async ({ page }) => {
    await row(page, "a_max").click();
    await page.locator(".table-wrap").press("Enter");
    await expect(traceTab(page)).toHaveCount(0);
    await expect(page.getByRole("tab", { name: "Details: a_max" })).toHaveAttribute("aria-selected", "true");
  });

  test("context menu, toolbar button and the Details link open it", async ({ page }) => {
    await row(page, "a_max").click();
    await expect(visualize(page)).toBeDisabled();
    await expect(visualize(page)).toHaveAttribute("title", "Visualize trace: select a CEX or cover");
    await row(page, "a_never9").click({ button: "right" });
    await page.getByRole("menuitem", { name: /Visualize trace/ }).click();
    await expect(traceTab(page)).toHaveText("Trace: a_never9");
    await page.getByRole("button", { name: "Close Trace: a_never9" }).click();

    await row(page, "c_done").click();
    await expect(visualize(page)).toBeEnabled();
    await visualize(page).click();
    await expect(traceTab(page)).toHaveText("Trace: c_done");
    await page.getByRole("button", { name: "Close Trace: c_done" }).click();

    await row(page, "a_never9").click();
    await page.getByRole("button", { name: "Trace: fdv_work/a_never9.vcd · 12 cycles" }).click();
    await expect(traceTab(page)).toHaveText("Trace: a_never9");
  });

  test("double-click on a task-tree leaf with a trace opens it", async ({ page }) => {
    await page.locator(".tree-row.leaf").filter({ hasText: "c_done" }).dblclick();
    await expect(traceTab(page)).toHaveText("Trace: c_done");
    await page.locator(".tree-row.leaf").filter({ hasText: "a_max" }).dblclick();
    await expect(traceTab(page)).toHaveText("Trace: c_done");
    await expect(page.getByRole("tab", { name: "Details: a_max" })).toHaveAttribute("aria-selected", "true");
  });

  test("selecting another property with a trace switches the tab", async ({ page }) => {
    await row(page, "a_never9").dblclick();
    await page.locator(".tree-row.leaf").filter({ hasText: "c_done" }).click();
    await expect(traceTab(page)).toHaveText("Trace: c_done");
    await page.locator(".tree-row.leaf").filter({ hasText: "a_max" }).click();
    await expect(traceTab(page)).toHaveText("Trace: c_done");
    await expect(visualize(page)).toBeDisabled();
  });

  test("cursor follows clicks and arrow keys; zoom changes the scale", async ({ page }) => {
    await row(page, "a_never9").dblclick();
    const svg = page.getByRole("img", { name: "Waveforms, 12 cycles" });
    await svg.click({ position: { x: 60 * 3 + 10, y: 60 } });
    await expect(cursor(page)).toHaveText("cycle 3");
    await expect(page.locator(".trace-name", { hasText: "counter.en" }).locator(".trace-val")).toHaveText("0");
    await page.keyboard.press("ArrowRight");
    await expect(cursor(page)).toHaveText("cycle 4");
    await expect(page.locator(".trace-name", { hasText: "counter.q" }).locator(".trace-val")).toHaveText("2");
    await page.keyboard.press("Home");
    await expect(cursor(page)).toHaveText("cycle 0");
    await page.keyboard.press("End");
    await expect(cursor(page)).toHaveText("cycle 11");
    await page.getByRole("button", { name: "Zoom in" }).click();
    await expect(svg).toHaveAttribute("width", String(12 * 80));
  });

  test("Add signal… adds a hidden signal", async ({ page }) => {
    await row(page, "a_never9").dblclick();
    await expect(page.locator(".trace-names .trace-name")).toHaveCount(1 + 1 + 5);
    await page.getByRole("button", { name: "Add signal…" }).click();
    await page.getByLabel("Filter signals").fill("cyc");
    await page.getByLabel("Filter signals").press("Enter");
    await expect(page.locator(".trace-names .trace-name")).toHaveCount(1 + 1 + 6);
    await expect(page.locator(".trace-name", { hasText: "fdv.fdv_cyc" }).locator(".trace-val")).toHaveText("b");
  });

  test("Export VCD downloads the trace", async ({ page }) => {
    await row(page, "a_never9").dblclick();
    const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export VCD" }).click()]);
    expect(download.suggestedFilename()).toBe("a_never9.vcd");
    const text = await (await download.createReadStream()).toArray();
    expect(Buffer.concat(text).toString()).toContain("$var reg 4 $ q [3:0] $end");
  });

  test("trace tab has no axe violations", async ({ page }) => {
    await row(page, "a_never9").dblclick();
    await expect(cursor(page)).toBeVisible();
    const r = await new AxeBuilder({ page }).include(".center").analyze();
    expect(r.violations.map((v) => v.id)).toEqual([]);
  });
});
