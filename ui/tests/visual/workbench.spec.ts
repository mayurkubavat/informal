import { test, expect, Page } from "@playwright/test";

// Screenshot baselines for the workbench in both themes at two window sizes.
// Regenerate with `npm run test:update` after an intended visual change.

const sizes = [
  { tag: "1440x900", viewport: { width: 1440, height: 900 } },
  { tag: "1000x700", viewport: { width: 1000, height: 700 } },
];
const schemes = ["light", "dark"] as const;

async function ready(page: Page) {
  await page.goto("/");
  await expect(page.locator("table.grid tbody tr")).toHaveCount(6);
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
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

      test("file menu open", async ({ page }) => {
        await ready(page);
        await page.getByRole("menuitem", { name: "File", exact: true }).click();
        await expect(page.locator(".menu-pop")).toBeVisible();
        await shot(page, n("file-menu"));
      });

      test("prove menu with shortcuts", async ({ page }) => {
        await ready(page);
        await page.getByRole("menuitem", { name: "Prove", exact: true }).click();
        await expect(page.locator(".menu-pop")).toBeVisible();
        await shot(page, n("prove-menu"));
      });
    });
