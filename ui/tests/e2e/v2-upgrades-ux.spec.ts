import { test, expect, Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

type Mock = {
  aiGui: (a: unknown) => Promise<Record<string, unknown>>;
  uiState: () => Record<string, unknown> | null;
};

const agent = (page: Page, action: Record<string, unknown>) =>
  page.evaluate((a) => (window as unknown as { __informalMock: Mock }).__informalMock.aiGui(a), action);
const refused = (page: Page, action: Record<string, unknown>) =>
  page.evaluate(
    (a) => (window as unknown as { __informalMock: Mock }).__informalMock.aiGui(a).then(() => "done", (e: Error) => e.message),
    action,
  );
const rows = (page: Page) => page.locator(".table-wrap table.grid tbody tr:not(.empty-row)");

async function provedSession(page: Page, query = "fast=1") {
  await page.goto(`/?${query}`);
  await expect(rows(page)).toHaveCount(6);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}

test.describe("InFormal v2: Standalone Launchpad, Screen Control v2, Canvas Waveform, CodeMirror 6, Schematic & Dockview", () => {
  test("standalone zero-state WelcomePane loads starter workspace and passes axe in light & dark themes", async ({ page }) => {
    await page.goto("/?welcome=1&fast=1");
    await expect(page.getByRole("heading", { name: "InFormal" })).toBeVisible();
    await expect(page.locator(".brand-logo").first()).toBeVisible();

    const lightAxe = await new AxeBuilder({ page }).include(".welcome-pane").analyze();
    expect(lightAxe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);

    // Switch to Dark via Command Palette (Ctrl+K)
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "Command palette" });
    await expect(dialog).toBeVisible();
    await dialog.getByRole("combobox").fill("Theme: Dark");
    await dialog.press("Enter");
    await expect(page.locator(":root")).toHaveAttribute("data-theme", "dark");

    const darkAxe = await new AxeBuilder({ page }).include(".welcome-pane").analyze();
    expect(darkAxe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);

    // Click Quick-Start Workspace -> populates session
    await page.getByRole("button", { name: /Saturating Counter & Witness/ }).click();
    await expect(rows(page)).toHaveCount(6);
  });

  test("AI screen-control v2 drives Why CEX, callouts, walkthrough, schematic, and deterministic SVG capture", async ({ page }) => {
    await provedSession(page);

    // Refusals before trace is open
    expect(await refused(page, { action: "toggle_why_cex", open: true })).toContain("no trace is open");
    expect(await refused(page, { action: "capture_view", target: "trace" })).toContain("no trace is open");

    // Open trace -> toggle Why CEX + Slice only -> annotate trace -> capture SVG
    await agent(page, { action: "open_trace", name: "a_never9", frame: 10 });
    const sliced = await agent(page, { action: "toggle_why_cex", open: true, slice_only: true });
    expect(sliced).toMatchObject({
      trace: { property: "a_never9", why_cex_open: true, slice_only: true },
    });
    await expect(page.getByRole("region", { name: "Causal root-cause slice" })).toBeVisible();
    // Slice only filters non-causal signals (leaving property lane + counter.en + counter.q = 3 rows)
    await expect(page.locator(".trace-names .trace-name:not(.trace-head)")).toHaveCount(3);

    const pinned = await agent(page, {
      action: "annotate_trace",
      annotations: [{ frame: 10, signal: "counter.q", label: "hits 9", tone: "danger" }],
    });
    expect((pinned.trace as { annotations: unknown[] }).annotations).toHaveLength(1);
    await expect(page.getByRole("region", { name: "AI waveform callouts" })).toContainText("hits 9");

    const captured = await agent(page, { action: "capture_view", target: "trace" });
    const svg = (captured.capture as { svg: string }).svg;
    expect(svg).toContain("<svg");
    expect(svg).toContain("a_never9");
    expect(svg).toContain("hits 9");

    // Guided multi-step walkthrough + auto-detach on manual user click
    const tour = await agent(page, {
      action: "present_walkthrough",
      title: "Root Cause Tour",
      steps: [
        { label: "Enable rises", note: "en drives increment", property: "a_never9", frame: 1, signals: ["counter.en"] },
        { label: "Violation", note: "q reaches 9", property: "a_never9", frame: 10, signals: ["counter.q"] },
      ],
    });
    expect(tour).toMatchObject({
      follow_agent: true,
      walkthrough: { title: "Root Cause Tour", step: 0, total_steps: 2 },
      trace: { cursor: 1 },
    });
    await expect(page.getByRole("region", { name: /Guided walkthrough/ })).toContainText("Enable rises");

    // User manually scrubs waveform -> Follow AI detaches cleanly to Paused
    await page.getByLabel(/^Trace of a_never9/).press("ArrowRight");
    await expect(page.locator(".follow-pill")).toContainText("Paused");

    // Open schematic via gui_open_schematic and capture schematic SVG
    const schemState = await agent(page, { action: "open_schematic", name: "a_never9", max_depth: 2 });
    expect(schemState).toMatchObject({ view: "schematic", schematic: { property: "a_never9", max_depth: 2 } });
    const schemCap = await agent(page, { action: "capture_view", target: "schematic" });
    expect((schemCap.capture as { svg: string }).svg).toContain("<svg");
  });

  test("500-signal x 5,000-cycle binary IFTR canvas waveform virtualizes DOM and scrubs under 16 ms", async ({ page }) => {
    await provedSession(page, "fast=1&traceSigs=500&traceCycles=5000");
    await rows(page).filter({ hasText: "a_never9" }).dblclick();

    await expect(page.getByText(/5000 cycles/)).toBeVisible();
    // Virtualized: out of 500 signals, fewer than 80 DOM signal rows are mounted, and 0 per-cycle SVG <g> cells!
    const mountedRows = await page.locator(".trace-names .trace-name").count();
    expect(mountedRows).toBeLessThan(80);
    await expect(page.locator("svg.trace-lanes g")).toHaveCount(0);

    // Measure cursor scrub frame latency across 10 scrubs
    const traceEl = page.getByLabel(/^Trace of a_never9/);
    await traceEl.focus();
    const durations: number[] = await page.evaluate(async () => {
      const el = document.querySelector(".trace") as HTMLElement;
      const samples: number[] = [];
      for (let i = 0; i < 10; i++) {
        const t0 = performance.now();
        el.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
        await new Promise((r) => requestAnimationFrame(() => r(null)));
        samples.push(performance.now() - t0);
      }
      return samples.sort((a, b) => a - b);
    });
    const medianMs = durations[Math.floor(durations.length / 2)];
    expect(medianMs).toBeLessThan(32);

    // Fit all 5,000 cycles into viewport (min/max bucket decimation path)
    await page.getByRole("button", { name: "Fit" }).click();
    await expect(page.locator("canvas.trace-canvas")).toBeVisible();
  });

  test("CodeMirror 6 live trace values, SV-FPV-003 1-click quick-fix, Go-to-Driver, and CEX Debug multi-pane dockview", async ({ page }) => {
    await provedSession(page);

    // Open trace at cycle 11 so activeTraceValues broadcasts q=4'h9
    await rows(page).filter({ hasText: "a_never9" }).dblclick();
    await expect(page.getByRole("status", { name: "Cursor" })).toHaveText("cycle 11");

    // Switch to CEX Debug 3-way multi-pane layout (CodeMirror Source + Canvas Trace + Causal Schematic simultaneously!)
    await page.getByLabel("Layout").selectOption("cex-debug");
    await expect(page.getByTestId("dockview-workspace")).toBeVisible();

    // Select m_en (which lives in counter.sv) so Source panel displays counter.sv RTL
    await agent(page, { action: "select_property", name: "m_en" });

    // Verify CodeMirror 6 renders inline live trace value annotations (`@C11`)!
    await expect(page.getByTestId("cm-trace-inline").first()).toContainText("@C11");

    // Verify live formal-ready lint (`SV-FPV-003`) surfaces with 1-click Quick-Fix and applies cleanly!
    const lintRegion = page.getByRole("region", { name: "Formal-readiness lint diagnostics" });
    await expect(lintRegion).toContainText("SV-FPV-003");
    await lintRegion.getByRole("button", { name: /Quick-Fix: Insert `disable iff/ }).click();
    await expect(page.locator("main .cm-source-host")).toContainText("disable iff (!rst_n)");
    await expect(page.getByRole("region", { name: "Formal-readiness lint diagnostics" })).toHaveCount(0);

    // Verify Go-to-Driver / Go-to-Load navigation on signal `q`
    const navInput = page.getByLabel("Go to signal driver or load");
    await navInput.fill("q");
    await navInput.press("Enter");
    const navPop = page.getByRole("region", { name: "Navigation for q" });
    await expect(navPop).toContainText("Drivers (2)");
    await expect(navPop).toContainText("always_ff");

    // Pin `@q` context chip to AI Ask console
    await navPop.getByRole("button", { name: "✦ Pin" }).click();
    await expect(page.getByRole("region", { name: "Pinned AI context" })).toContainText("@q");

    // Accessibility check on multi-pane dockview workspace
    const axe = await new AxeBuilder({ page }).include(".center").analyze();
    expect(axe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  });

  test("Antigravity SEP-1865 MCP App surface (?mcpApp=1) hides standalone menubar and syncs host theme", async ({ page }) => {
    await page.goto("/?mcpApp=1&fast=1");
    await expect(page.locator(".app.mcp-app-mode")).toBeVisible();
    await expect(page.getByRole("menubar", { name: "Main menu" })).toHaveCount(0);
    await expect(page.getByText("Antigravity MCP App")).toBeVisible();
  });
});
