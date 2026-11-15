import { test, expect, Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

// Ask mode against the browser mock's Fake provider (src/mock.ts): any
// question gets "Proving only that property:" plus `prove -property
// a_never9`; a question about failures is answered from the session.
// "ollama-gemma" is always offline in the browser; ?llm=offline starts so.

const statusbar = (page: Page) => page.locator("footer.statusbar");
const consoleLog = (page: Page) => page.locator(".console-log");
const askSwitch = (page: Page) => page.getByRole("group", { name: "Console input mode" }).getByRole("button", { name: "Ask" });
const tclSwitch = (page: Page) => page.getByRole("group", { name: "Console input mode" }).getByRole("button", { name: "Tcl" });
const row = (page: Page, name: string) =>
  page.locator("table.grid tbody tr:not(.empty-row)").filter({ has: page.getByRole("cell", { name, exact: true }) });

async function open(page: Page, query = "fast=1") {
  await page.goto(`/?${query}`);
  await expect(page.locator("table.grid tbody tr:not(.empty-row)")).toHaveCount(6);
}
async function ask(page: Page, q: string) {
  await askSwitch(page).click();
  const input = page.getByLabel("Ask a question");
  await input.fill(q);
  await input.press("Enter");
}

test("status bar names the provider; the switch starts on Tcl", async ({ page }) => {
  await open(page);
  await expect(statusbar(page)).toContainText("LLM: fake/scripted");
  await expect(tclSwitch(page)).toHaveAttribute("aria-pressed", "true");
  await expect(askSwitch(page)).toBeEnabled();
  await expect(page.locator(".prompt")).toHaveText("informal[jg]>");
});

test("switch, View menu and Ctrl+Shift+A change the input mode", async ({ page }) => {
  await open(page);
  await askSwitch(page).click();
  await expect(page.locator(".prompt")).toHaveText("ask>");
  await expect(page.getByLabel("Ask a question")).toBeFocused();
  await page.keyboard.press("Control+Shift+A");
  await expect(page.locator(".prompt")).toHaveText("informal[jg]>");
  await page.keyboard.press("Control+Shift+A");
  await expect(page.locator(".prompt")).toHaveText("ask>");
  await page.getByRole("menuitem", { name: "View", exact: true }).click();
  await page.getByRole("menuitem", { name: /Input mode: Tcl/ }).click();
  await expect(page.locator(".prompt")).toHaveText("informal[jg]>");
});

test("ask streams plain ask: lines and pre-fills the suggested command; nothing runs", async ({ page }) => {
  await open(page);
  await ask(page, "prove only the a_never9 property");
  await expect(consoleLog(page)).toContainText("ask> prove only the a_never9 property");
  await expect(consoleLog(page).locator(".line.ask")).toHaveText("ask: Proving only that property:\nask: prove -property a_never9");
  const input = page.getByLabel("Suggested Tcl command");
  await expect(input).toHaveValue("prove -property a_never9");
  await expect(page.locator(".prompt")).toHaveText("informal[jg]>");
  await expect(page.locator(".prompt-hint")).toHaveText("Enter runs · Esc discards");
  // Not run: no echo, a_never9 still unprocessed.
  await expect(consoleLog(page)).not.toContainText("informal[jg]> prove");
  await expect(row(page, "a_never9")).not.toContainText("CEX");
});

test("Enter runs the suggestion as a normal Tcl command, then the prompt is ask> again", async ({ page }) => {
  await open(page);
  await ask(page, "prove only the a_never9 property");
  const input = page.getByLabel("Suggested Tcl command");
  await expect(input).toHaveValue("prove -property a_never9");
  await input.press("Enter");
  await expect(consoleLog(page)).toContainText("informal[jg]> prove -property a_never9");
  await expect(row(page, "a_never9")).toContainText("CEX");
  await expect(row(page, "a_max")).not.toContainText("Proven");
  await expect(page.locator(".prompt")).toHaveText("ask>");
  await expect(page.getByLabel("Ask a question")).toHaveValue("");
});

test("Esc discards the suggestion without running it", async ({ page }) => {
  await open(page);
  await ask(page, "prove only the a_never9 property");
  const input = page.getByLabel("Suggested Tcl command");
  await expect(input).toHaveValue("prove -property a_never9");
  await input.press("Escape");
  await expect(page.locator(".prompt")).toHaveText("ask>");
  await expect(page.getByLabel("Ask a question")).toHaveValue("");
  await expect(page.locator(".prompt-hint")).toHaveCount(0);
  await expect(consoleLog(page)).not.toContainText("informal[jg]> prove");
  await expect(row(page, "a_never9")).not.toContainText("CEX");
});

test("a question gets an answer and no command", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(statusbar(page)).toContainText("Idle");
  await ask(page, "which properties failed?");
  await expect(consoleLog(page).locator(".line.ask").last()).toHaveText("ask: One property failed: a_never9 (CEX).");
  await expect(page.locator(".prompt")).toHaveText("ask>");
  await expect(page.locator(".prompt-hint")).toHaveCount(0);
});

test("Esc cancels a streaming answer", async ({ page }) => {
  await open(page, "tick=300");
  await ask(page, "prove only the a_never9 property");
  await expect(page.getByLabel("Ask a question")).toHaveAttribute("placeholder", "answering… (Esc cancels)");
  await page.getByLabel("Ask a question").press("Escape");
  await expect(consoleLog(page)).toContainText("ask: cancelled");
  await expect(page.getByLabel("Suggested Tcl command")).toHaveCount(0);
});

test("offline: Ask is disabled with the error as tooltip, and the status bar says offline", async ({ page }) => {
  await open(page, "fast=1&llm=offline");
  await expect(statusbar(page)).toContainText("LLM: offline");
  await expect(askSwitch(page)).toBeDisabled();
  await expect(askSwitch(page)).toHaveAttribute("title", /Connection refused/);
  await page.keyboard.press("Control+Shift+A");
  await expect(page.locator(".prompt")).toHaveText("informal[jg]>");
  await page.getByRole("menuitem", { name: "View", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: /Input mode: Ask/ })).toBeDisabled();
});

test("status bar menu switches provider; an offline one disables Ask, back again enables it", async ({ page }) => {
  await open(page);
  await askSwitch(page).click();
  await statusbar(page).getByRole("button", { name: /LLM:/ }).click();
  const menu = page.getByRole("menu", { name: "LLM provider" });
  await expect(menu.getByRole("menuitem")).toHaveText([/fake.*scripted/, /ollama-gemma.*gemma3:12b/, "Check connection"]);
  await menu.getByRole("menuitem", { name: /ollama-gemma/ }).click();
  await expect(statusbar(page)).toContainText("LLM: offline");
  await expect(askSwitch(page)).toBeDisabled();
  await expect(page.locator(".prompt")).toHaveText("informal[jg]>");
  await statusbar(page).getByRole("button", { name: /LLM:/ }).click();
  await page.getByRole("menu", { name: "LLM provider" }).getByRole("menuitem", { name: /fake/ }).click();
  await expect(statusbar(page)).toContainText("LLM: fake/scripted");
  await expect(askSwitch(page)).toBeEnabled();
});

test("Ask mode passes an accessibility scan", async ({ page }) => {
  await open(page);
  await ask(page, "prove only the a_never9 property");
  await expect(page.getByLabel("Suggested Tcl command")).toHaveValue("prove -property a_never9");
  const r = await new AxeBuilder({ page }).include(".bottom").include("footer.statusbar").analyze();
  expect(r.violations.map((v) => v.id)).toEqual([]);
});

test("a why question shows what the model looked up, above its answer", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator(".strip-summary")).toContainText("2 proven");
  await ask(page, "why does a_never9 fail?");
  const lines = consoleLog(page).locator(".line");
  await expect(consoleLog(page).locator(".line.ask")).toContainText("a_never9 fails: q reaches 9");
  await expect(consoleLog(page).locator(".line.ask-step")).toHaveText([
    "ask: looked up property_list status=cex",
    "ask: looked up trace_get name=a_never9",
  ]);
  // Question, lookups, answer: in that order, at the end of the log.
  const tail = (await lines.allTextContents()).slice(-4);
  expect(tail[0]).toBe("ask> why does a_never9 fail?");
  expect(tail[1]).toContain("property_list");
  expect(tail[3]).toContain("a_never9 fails");
  // Looking up is not doing: no command is suggested, the prompt stays in Ask.
  await expect(page.locator(".prompt")).toHaveText("ask>");
});
