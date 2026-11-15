import { test, expect, Page } from "@playwright/test";

// An agent's window tools (docs/ai-interface.md): gui_state reads what the
// views report, and the gui_ actions reach the views as "ai-gui" requests.
// The browser mock plays the agent through window.__informalMock: aiGui
// resolves with the window's state after the action, as the tool's reply
// would, or rejects with the views' reason.

type Mock = { aiGui: (a: unknown) => Promise<Record<string, unknown>>; uiState: () => Record<string, unknown> | null };
const agent = (page: Page, action: Record<string, unknown>) =>
  page.evaluate((a) => (window as unknown as { __informalMock: Mock }).__informalMock.aiGui(a), action);
const refused = (page: Page, action: Record<string, unknown>) =>
  page.evaluate(
    (a) => (window as unknown as { __informalMock: Mock }).__informalMock.aiGui(a).then(() => "done", (e: Error) => e.message),
    action,
  );
const seen = (page: Page) => page.evaluate(() => (window as unknown as { __informalMock: Mock }).__informalMock.uiState());
const rows = (page: Page) => page.locator(".table-wrap table.grid tbody tr:not(.empty-row)");
const tab = (page: Page, name: string | RegExp) => page.locator(".bottom").getByRole("tab", { name });

async function proved(page: Page) {
  await page.goto("/?fast=1");
  await expect(rows(page)).toHaveCount(6);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator(".strip-summary")).toContainText("2 proven");
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}

test("the window reports what the person selected and is looking at", async ({ page }) => {
  await proved(page);
  await expect.poll(() => seen(page)).toMatchObject({ selected_property: null, view: "table", trace: null });
  await rows(page).filter({ hasText: "a_never9" }).click();
  await expect.poll(() => seen(page)).toMatchObject({ selected_property: "a_never9", view: "table" });
  await page.getByLabel("Filter properties").fill("a_");
  await expect.poll(() => seen(page)).toMatchObject({ filter: "a_" });
  await rows(page).filter({ hasText: "a_never9" }).dblclick();
  await expect
    .poll(() => seen(page))
    .toMatchObject({ view: "trace", trace: { property: "a_never9", cursor: 11, signals: ["counter.clk", "counter.rst_n", "counter.en", "counter.q", "counter.done"] } });
  await page.getByLabel(/^Trace of a_never9/).press("ArrowLeft");
  await expect.poll(() => seen(page)).toMatchObject({ trace: { cursor: 10 } });
});

test("select a property: the row is selected and the console keeps the dock", async ({ page }) => {
  await proved(page);
  const state = await agent(page, { action: "select_property", name: "a_step" });
  expect(state).toMatchObject({ selected_property: "a_step", view: "table" });
  await expect(rows(page).filter({ hasText: "a_step" })).toHaveAttribute("aria-selected", "true");
  await expect(tab(page, "Console")).toHaveAttribute("aria-selected", "true");
  expect(await refused(page, { action: "select_property", name: "nope" })).toBe("no property named nope");
  // What moved the window is on record.
  await tab(page, /Messages/).click();
  await expect(page.locator(".messages")).toContainText("AI window: select a_step");
  await expect(page.locator(".messages")).toContainText("no property named nope");
});

test("open a trace at a cycle, then add signals to it", async ({ page }) => {
  await proved(page);
  expect(await refused(page, { action: "add_signals", signals: ["counter.q"] })).toContain("no trace is open");
  expect(await refused(page, { action: "open_trace", name: "a_max" })).toBe("a_max has no trace: its status is proven");
  const state = await agent(page, { action: "open_trace", name: "a_never9", frame: 4 });
  expect(state).toMatchObject({ selected_property: "a_never9", view: "trace", trace: { property: "a_never9", cursor: 4 } });
  await expect(page.getByRole("status", { name: "Cursor" })).toHaveText("cycle 4");
  // Without a frame the cursor stays where the person has it.
  await page.getByLabel(/^Trace of a_never9/).press("ArrowRight");
  expect(await agent(page, { action: "open_trace", name: "a_never9" })).toMatchObject({ trace: { cursor: 5 } });
  // A helper signal is not shown until asked for.
  const names = page.locator(".trace-names .trace-name .ellipsis");
  await expect(names.filter({ hasText: "informal.informal_cyc" })).toHaveCount(0);
  const added = await agent(page, { action: "add_signals", signals: ["informal.informal_cyc", "counter.q"] });
  expect((added.trace as { signals: string[] }).signals).toContain("informal.informal_cyc");
  await expect(names.filter({ hasText: "informal.informal_cyc" })).toHaveCount(1);
  expect(await refused(page, { action: "add_signals", signals: ["counter.nosuch", "counter.en"] })).toBe("not in this trace: counter.nosuch");
});

test("show source lines: the file opens with the range highlighted until the person selects a property", async ({ page }) => {
  await proved(page);
  const state = await agent(page, { action: "highlight_source", file: "tests/fpv/counter/counter.sv", first_line: 4, last_line: 6 });
  expect(state).toMatchObject({ view: "source", source: { file: "tests/fpv/counter/counter.sv", first_line: 4, last_line: 6 } });
  const marked = page.locator(".center .source div.focus");
  await expect(marked).toHaveCount(3);
  await expect(marked.first()).toContainText("always_ff");
  expect(await refused(page, { action: "highlight_source", file: "no/such.sv", first_line: 1, last_line: 1 })).toContain("no such file");
  // The person's own selection takes the source view back.
  await page.locator(".center").getByRole("tab", { name: "Property Table" }).click();
  await rows(page).filter({ hasText: "a_never9" }).click();
  await expect.poll(() => seen(page)).toMatchObject({ source: { file: "tests/fpv/counter/counter.jg.tcl", first_line: 10, last_line: 10 } });
});
