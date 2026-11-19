import { test, expect, Page } from "@playwright/test";

// Screenshot baselines for the workbench in both themes at two window sizes.
// Regenerate with `npm run test:update` after an intended visual change.

const sizes = [
  { tag: "1440x900", viewport: { width: 1440, height: 900 } },
  { tag: "1000x700", viewport: { width: 1000, height: 700 } },
];
const schemes = ["light", "dark"] as const;

async function ready(page: Page, query = "fast=1") {
  await page.goto(`/?${query}`);
  await expect(page.locator("table.grid tbody tr")).toHaveCount(6);
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}
// A prove held mid-run by the mock (?freeze=2): a_max proven, a_step and
// a_never9 running, c_done and c_10 queued; times are nominal, so the frame
// is the same on every run.
async function midRun(page: Page, query = "") {
  await ready(page, `freeze=2${query}`);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator("footer.statusbar")).toContainText("Proving · 1/5");
}
async function proveAll(page: Page) {
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator(".strip-summary")).toContainText("2 proven");
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}
const shot = (page: Page, name: string) =>
  expect(page).toHaveScreenshot(name, {
    // Wall-clock times and durations in Messages change on every run.
    mask: [page.locator(".messages td:first-child"), page.locator(".messages td:last-child")],
  });

for (const s of sizes)
  for (const scheme of schemes)
    test.describe(`${scheme} ${s.tag}`, () => {
      test.use({ viewport: s.viewport, colorScheme: scheme });
      const n = (what: string) => `${what}-${scheme}-${s.tag}.png`;

      test("initial", async ({ page }) => {
        await ready(page);
        await shot(page, n("initial"));
      });

      test("after prove all", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await shot(page, n("proved"));
      });

      test("details open", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.locator("table.grid tbody tr").filter({ hasText: "a_never9" }).click();
        await expect(page.locator(".details pre.expr")).toBeVisible();
        await shot(page, n("details"));
      });

      test("log tab and completion list", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.keyboard.press("Control+4");
        await expect(page.getByRole("log", { name: "Engine output" })).toContainText("unsat");
        await shot(page, n("log"));
        await page.keyboard.press("Control+1");
        await page.getByLabel("Tcl command").fill("prove -property a_");
        await page.getByLabel("Tcl command").press("Tab");
        await expect(page.getByRole("listbox", { name: "Completions" })).toBeVisible();
        await shot(page, n("completion"));
      });

      test("AI proposals", async ({ page }) => {
        await ready(page, "fast=1&ai=1");
        await page.locator(".bottom").getByRole("tab", { name: "Proposals (2)" }).click();
        await page.getByRole("button", { name: "Accept Assumption m_en_hold" }).click();
        await expect(page.locator(".table-wrap table.grid tbody tr").filter({ hasText: "m_en_hold" })).toHaveCount(1);
        await expect(page.locator("footer.statusbar")).toContainText("Idle");
        await shot(page, n("proposals"));
      });

      test("profiler tab", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.locator("table.grid tbody tr").filter({ hasText: "a_never9" }).click();
        await page.keyboard.press("Control+5");
        await expect(page.locator(".profiler tbody tr")).toHaveCount(7);
        await shot(page, n("profiler"));
      });

      test("design browser", async ({ page }) => {
        await ready(page, "fast=1&design=fifo");
        await page.getByRole("tab", { name: "Design", exact: true }).click();
        await page.getByRole("treeitem").filter({ hasText: "u_wp" }).click();
        await expect(page.locator(".design-head")).toContainText("fifo.u_wp");
        await shot(page, n("design"));
      });

      test("source tab", async ({ page }) => {
        await ready(page);
        await page.getByRole("tab", { name: "counter.sv" }).click();
        await expect(page.locator("main pre.source")).toBeVisible();
        await shot(page, n("source"));
      });

      test("row context menu", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.locator("table.grid tbody tr").filter({ hasText: "a_never9" }).click({ button: "right", position: { x: 120, y: 12 } });
        await expect(page.getByRole("menu", { name: "Actions for a_never9" })).toBeVisible();
        await shot(page, n("context-menu"));
      });

      test("filtered with match count", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.getByLabel("Filter properties").fill("bmc");
        await expect(page.locator(".filter-count")).toHaveText("2 of 6");
        await page.getByLabel("Filter properties").blur();
        await shot(page, n("filtered"));
      });

      test("restored custom layout", async ({ page }) => {
        await page.addInitScript(() =>
          localStorage.setItem("informal-layout", JSON.stringify({ leftW: 320, bottomH: 320, left: "design", center: "table", bottom: "console", leftOpen: true })),
        );
        await ready(page);
        await expect(page.getByRole("tab", { name: "Design", exact: true })).toHaveAttribute("aria-selected", "true");
        await shot(page, n("layout"));
      });

      test("file menu open", async ({ page }) => {
        await ready(page);
        await page.getByRole("menuitem", { name: "File", exact: true }).click();
        await expect(page.locator(".menu-pop")).toBeVisible();
        await shot(page, n("file-menu"));
      });

      test("prove running", async ({ page }) => {
        await midRun(page);
        await expect(page.locator(".progress-text")).toContainText("1 of 5 · 0:00 · ");
        await shot(page, n("running"));
      });

      test("prove stopping", async ({ page }) => {
        await midRun(page, "&holdStop=1");
        await page.getByRole("button", { name: /^Stop/ }).click();
        await expect(page.getByRole("button", { name: "Stopping…" })).toBeDisabled();
        await shot(page, n("stopping"));
      });

      test("trace tab", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.locator("table.grid tbody tr").filter({ hasText: "a_never9" }).dblclick();
        await expect(page.getByRole("status", { name: "Cursor" })).toHaveText("cycle 11");
        await shot(page, n("trace"));
      });

      test("trace tab, cover with an added signal", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.locator("table.grid tbody tr").filter({ hasText: "c_done" }).dblclick();
        await page.getByRole("button", { name: "Add signal…" }).click();
        await page.getByLabel("Filter signals").fill("cyc");
        await page.getByLabel("Filter signals").press("Enter");
        await page.getByLabel("Radix of counter.q").selectOption("bin");
        await page.getByRole("img", { name: /^Waveforms/ }).click({ position: { x: 60 * 4 + 20, y: 40 } });
        await expect(page.getByRole("status", { name: "Cursor" })).toHaveText("cycle 4");
        await shot(page, n("trace-cover"));
      });

      test("prove menu with shortcuts", async ({ page }) => {
        await ready(page);
        await page.getByRole("menuitem", { name: "Prove", exact: true }).click();
        await expect(page.locator(".menu-pop")).toBeVisible();
        await shot(page, n("prove-menu"));
      });

      // Ask mode (Fake provider): an answered question, then a suggested
      // command waiting on the prompt.
      test("ask mode with a suggested command", async ({ page }) => {
        await ready(page);
        await proveAll(page);
        await page.getByRole("group", { name: "Console input mode" }).getByRole("button", { name: "Ask" }).click();
        const q = page.getByLabel("Ask a question");
        await q.fill("which properties failed?");
        await q.press("Enter");
        await expect(page.locator(".line.ask").last()).toContainText("One property failed");
        await page.getByLabel("Ask a question").fill("prove only the a_never9 property");
        await page.getByLabel("Ask a question").press("Enter");
        await expect(page.getByLabel("Suggested Tcl command")).toHaveValue("prove -property a_never9");
        await shot(page, n("ask-suggested"));
      });

      test("ask offline", async ({ page }) => {
        await ready(page, "fast=1&llm=offline");
        await expect(page.locator("footer.statusbar")).toContainText("LLM: offline");
        await shot(page, n("ask-offline"));
      });

      test("llm provider menu", async ({ page }) => {
        await ready(page);
        await page.locator("footer.statusbar").getByRole("button", { name: /LLM:/ }).click();
        await expect(page.getByRole("menu", { name: "LLM provider" })).toBeVisible();
        await shot(page, n("llm-menu"));
      });
    });
