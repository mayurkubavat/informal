import { test, expect, Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

// The AI interface in the workbench (docs/ai-interface.md): with ?ai=1 the
// browser mock starts as if an agent on the app's socket had proposed an
// assumption (m_en_hold) and a helper assertion (a_q_bound). Nothing is in
// the session until the person accepts.

const statusbar = (page: Page) => page.locator("footer.statusbar");
const rows = (page: Page) => page.locator(".table-wrap table.grid tbody tr:not(.empty-row)");
const tab = (page: Page, name: string | RegExp) => page.locator(".bottom").getByRole("tab", { name });
const cards = (page: Page) => page.getByRole("list", { name: "AI proposals" }).getByRole("listitem");

async function open(page: Page, query = "fast=1&ai=1") {
  await page.goto(`/?${query}`);
  await expect(rows(page)).toHaveCount(6);
}

test("without proposals there is no Proposals tab and no notice", async ({ page }) => {
  await open(page, "fast=1");
  await expect(tab(page, /Proposals/)).toHaveCount(0);
  await expect(statusbar(page)).not.toContainText("proposal");
});

test("pending proposals are announced, not applied; the notice opens the tab", async ({ page }) => {
  await open(page);
  await expect(tab(page, "Proposals (2)")).toBeVisible();
  // The console keeps the dock: nothing steals the person's place.
  await expect(tab(page, "Console")).toHaveAttribute("aria-selected", "true");
  await expect(rows(page).filter({ hasText: "m_en_hold" })).toHaveCount(0);
  await statusbar(page).getByRole("button", { name: "AI: 2 proposals waiting" }).click();
  await expect(tab(page, "Proposals (2)")).toHaveAttribute("aria-selected", "true");
  await expect(cards(page)).toHaveCount(2);
  const first = cards(page).first();
  await expect(first).toContainText("Assumption");
  await expect(first).toContainText("m_en_hold");
  await expect(first.locator("pre.expr")).toHaveText("en |=> en");
  await expect(first).toContainText("Waiting for you");
});

test("accepting adds the property, tagged AI; rejecting adds nothing", async ({ page }) => {
  await open(page);
  await tab(page, "Proposals (2)").click();
  await page.getByRole("button", { name: "Accept Assumption m_en_hold" }).click();
  const added = rows(page).filter({ hasText: "m_en_hold" });
  await expect(added).toHaveCount(1);
  await expect(added.getByTitle("Added through the AI interface")).toHaveText("AI");
  await expect(tab(page, "Proposals (1)")).toBeVisible();
  await expect(statusbar(page)).toContainText("AI: 1 proposal waiting");
  await page.getByRole("button", { name: "Reject Assertion a_q_bound" }).click();
  await expect(tab(page, /Proposals/)).toHaveText("Proposals");
  await expect(rows(page).filter({ hasText: "a_q_bound" })).toHaveCount(0);
  await expect(statusbar(page)).not.toContainText("waiting");
  // Decided cards stay, newest first, with no answers left to give.
  await expect(cards(page)).toHaveCount(2);
  await expect(cards(page).nth(0)).toContainText("Rejected");
  await expect(cards(page).nth(1)).toContainText("Accepted");
  await expect(page.getByRole("button", { name: /^(Accept|Reject) / })).toHaveCount(0);
  // Both answers are on record.
  await tab(page, /Messages/).click();
  await expect(page.locator(".messages")).toContainText("accept AI proposal: assumption m_en_hold");
  await expect(page.locator(".messages")).toContainText("reject AI proposal: assertion a_q_bound");
  // The accepted property's details say where it came from.
  await added.click();
  await tab(page, /Details/).click();
  await expect(page.locator(".details")).toContainText("Added through the AI interface");
});

test("the Proposals tab passes the accessibility checks", async ({ page }) => {
  await open(page);
  await tab(page, "Proposals (2)").click();
  await expect(cards(page)).toHaveCount(2);
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
});
