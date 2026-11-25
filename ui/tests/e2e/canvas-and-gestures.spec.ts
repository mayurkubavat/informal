// Pillar 3 — `<canvas>` Pixel / HiDPI / Hit-Test, Desktop IDE Pointer Gestures,
// Keyboard Focus Restoration & Whole-Document Accessibility Audit.
//
// Runs on BOTH 1x Chromium (`e2e`) AND 2x Retina WebKit (`webkit-hidpi`), verifying
// real rendered pixels on Waveform and ELK Schematic canvases, pointer-drag splitters
// with localStorage persistence, Dockview panel switching, and focus return on Escape.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";

interface CanvasInkStats {
  cssW: number;
  cssH: number;
  bufW: number;
  bufH: number;
  dpr: number;
  inkRatio: number;
  bgHex: string;
}

async function inspectCanvasInk(canvas: Locator): Promise<CanvasInkStats> {
  return canvas.evaluate((el) => {
    const cvs = el as HTMLCanvasElement;
    const rect = cvs.getBoundingClientRect();
    const ctx = cvs.getContext("2d");
    if (!ctx || cvs.width === 0 || cvs.height === 0) {
      return {
        cssW: rect.width,
        cssH: rect.height,
        bufW: cvs.width,
        bufH: cvs.height,
        dpr: window.devicePixelRatio || 1,
        inkRatio: 0,
        bgHex: "#000000",
      };
    }
    const { data, width, height } = ctx.getImageData(0, 0, cvs.width, cvs.height);
    // Sample bottom-right corner as background reference (away from ruler header & left margin)
    const refIdx = ((height - 2) * width + (width - 2)) * 4;
    const br = data[refIdx];
    const bg = data[refIdx + 1];
    const bb = data[refIdx + 2];
    let diff = 0;
    let total = 0;
    const step = 4; // stride every 4th pixel for fast deterministic sampling
    for (let y = 0; y < height; y += step) {
      for (let x = 0; x < width; x += step) {
        const i = (y * width + x) * 4;
        const delta = Math.abs(data[i] - br) + Math.abs(data[i + 1] - bg) + Math.abs(data[i + 2] - bb);
        if (delta > 12) diff++;
        total++;
      }
    }
    const hex = (n: number) => n.toString(16).padStart(2, "0");
    return {
      cssW: rect.width,
      cssH: rect.height,
      bufW: cvs.width,
      bufH: cvs.height,
      dpr: window.devicePixelRatio || 1,
      inkRatio: total > 0 ? diff / total : 0,
      bgHex: `#${hex(br)}${hex(bg)}${hex(bb)}`,
    };
  });
}

async function sampleCanvasBytes(canvas: Locator): Promise<number[]> {
  return canvas.evaluate((el) => {
    const cvs = el as HTMLCanvasElement;
    const ctx = cvs.getContext("2d");
    if (!ctx || cvs.width === 0 || cvs.height === 0) return [];
    const { data, width, height } = ctx.getImageData(0, 0, cvs.width, cvs.height);
    const out: number[] = [];
    const step = Math.max(4, Math.floor(Math.sqrt((width * height) / 4096)));
    for (let y = 0; y < height; y += step) {
      for (let x = 0; x < width; x += step) {
        const i = (y * width + x) * 4;
        out.push(data[i], data[i + 1], data[i + 2]);
      }
    }
    return out;
  });
}

function countDiffSamples(a: number[], b: number[]): number {
  const n = Math.min(a.length, b.length);
  let changed = 0;
  for (let i = 0; i + 2 < n; i += 3) {
    if (Math.abs(a[i] - b[i]) + Math.abs(a[i + 1] - b[i + 1]) + Math.abs(a[i + 2] - b[i + 2]) > 8) {
      changed++;
    }
  }
  return changed;
}

async function proveDemo(page: Page) {
  await page.goto("/?fast=1");
  await expect(page.locator(".table-wrap table.grid tbody tr:not(.empty-row)")).toHaveCount(6);
  await page.getByRole("button", { name: "Prove all" }).click();
  await expect(page.locator(".strip-summary")).toContainText("1 cex");
  await expect(page.locator("footer.statusbar")).toContainText("Idle");
}

async function switchTheme(page: Page, label: "Theme: Dark" | "Theme: Light") {
  await page.keyboard.press("Control+k");
  const dlg = page.getByRole("dialog", { name: "Command palette" });
  await expect(dlg).toBeVisible();
  await dlg.getByRole("combobox").fill(label);
  await dlg.press("Enter");
  await expect(dlg).toHaveCount(0);
}

test.describe("Canvas HiDPI Pixels, Hit-Testing, Pointer Gestures & A11y Focus", () => {
  test("Waveform <canvas> matches HiDPI backing scale, draws contrasting ink in Light & Dark, and click-to-scrub shifts pixels", async ({
    page,
  }) => {
    await proveDemo(page);

    // Open CEX waveform trace for a_never9
    const cexRow = page.locator(".table-wrap table.grid tbody tr", { hasText: "a_never9" });
    await cexRow.dblclick();
    const canvas = page.locator("canvas.trace-canvas");
    await expect(canvas).toBeVisible();
    await expect(page.locator(".trace-cursor")).toContainText("cycle 11");

    // 1. HiDPI backing-store check (exact 1x on Chromium, exact 2x Retina on WebKit)
    const lightInk = await inspectCanvasInk(canvas);
    expect(lightInk.bufW).toBe(Math.round(lightInk.cssW * lightInk.dpr));
    expect(lightInk.bufH).toBe(Math.round(lightInk.cssH * lightInk.dpr));
    expect(lightInk.inkRatio).toBeGreaterThan(0.01);

    // 2. Click-to-scrub hit-test on svg.trace-lanes updates cursor cycle AND shifts rendered canvas pixels
    const beforeScrub = await sampleCanvasBytes(canvas);
    const pxText = (await page.locator(".trace-bar .zoom-text").textContent()) || "64px";
    const pxPerCycle = Number.parseFloat(pxText);
    expect(pxPerCycle).toBeGreaterThan(0);
    const lanes = page.locator("svg.trace-lanes");
    const box = await lanes.boundingBox();
    expect(box).not.toBeNull();
    await page.mouse.click(box!.x + 2.5 * pxPerCycle, box!.y + 44);
    await expect(page.locator(".trace-cursor")).toContainText("cycle 2");
    const afterScrub = await sampleCanvasBytes(canvas);
    expect(countDiffSamples(beforeScrub, afterScrub)).toBeGreaterThan(20);

    // 3. Flip to Dark theme -> canvas repaints with dark surface and contrasting waveform ink
    await switchTheme(page, "Theme: Dark");
    await expect(page.locator(":root")).toHaveAttribute("data-theme", "dark");
    await expect
      .poll(async () => (await inspectCanvasInk(canvas)).bgHex)
      .not.toBe(lightInk.bgHex);
    const darkInk = await inspectCanvasInk(canvas);
    expect(darkInk.inkRatio).toBeGreaterThan(0.01);
  });

  test("ELK Schematic <canvas> matches HiDPI backing scale, renders non-blank graph, responds to mouse drag-pan & Ctrl-wheel zoom, and inspects nodes", async ({
    page,
  }) => {
    await proveDemo(page);

    await page.locator(".table-wrap table.grid tbody tr", { hasText: "a_never9" }).click();
    await page.getByRole("button", { name: "COI & causal schematic" }).click();
    const canvas = page.locator(".schematic-pane canvas");
    await expect(canvas).toBeVisible();
    await expect(page.locator(".schematic-bar .strip-summary")).toContainText("a_never9");

    // 1. HiDPI & non-blank ink verification
    const lightInk = await inspectCanvasInk(canvas);
    expect(lightInk.bufW).toBe(Math.round(lightInk.cssW * lightInk.dpr));
    expect(lightInk.bufH).toBe(Math.round(lightInk.cssH * lightInk.dpr));
    expect(lightInk.inkRatio).toBeGreaterThan(0.01);

    // 2. Real pointer drag-pan over canvas shifts rendered pixels
    const beforePan = await sampleCanvasBytes(canvas);
    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();
    const cx = box!.x + box!.width / 2;
    const cy = box!.y + box!.height / 2;
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    await page.mouse.move(cx + 95, cy + 55, { steps: 6 });
    await page.mouse.up();
    const afterPan = await sampleCanvasBytes(canvas);
    expect(countDiffSamples(beforePan, afterPan)).toBeGreaterThan(30);

    // 3. Ctrl+wheel zoom changes scale & repaints canvas
    await page.locator(".schematic-canvas-wrap").evaluate((el) => {
      el.dispatchEvent(new WheelEvent("wheel", { deltaY: -120, ctrlKey: true, bubbles: true, cancelable: true }));
    });
    const afterZoom = await sampleCanvasBytes(canvas);
    expect(countDiffSamples(afterPan, afterZoom)).toBeGreaterThan(30);

    // 4. Accessible keyboard node selection opens inspector with Jump to Source / Add to Trace
    await page.getByRole("button", { name: "Fit", exact: true }).click();
    const firstNodeBtn = page.locator('.schematic-canvas-wrap ul[aria-label="Schematic nodes"] button').first();
    await firstNodeBtn.focus();
    await page.keyboard.press("Enter");
    const inspector = page.getByRole("region", { name: "Selected schematic node" });
    await expect(inspector).toBeVisible();
    await expect(inspector.getByRole("button", { name: "Jump to Source" })).toBeVisible();

    // 5. Dark theme repaint check
    await switchTheme(page, "Theme: Dark");
    await expect
      .poll(async () => (await inspectCanvasInk(canvas)).bgHex)
      .not.toBe(lightInk.bgHex);
    expect((await inspectCanvasInk(canvas)).inkRatio).toBeGreaterThan(0.01);
  });

  test("Desktop IDE pointer splitters drag-resize, persist in localStorage across reload, double-click reset, and CEX Debug Dockview switches cleanly", async ({
    page,
  }) => {
    await proveDemo(page);

    const leftPane = page.locator("aside.left");
    const bottomPane = page.locator("section.bottom");
    const vSplitter = page.locator(".vsplitter");
    const hSplitter = page.locator(".splitter");

    const w0 = (await leftPane.boundingBox())!.width;
    const h0 = (await bottomPane.boundingBox())!.height;

    // 1. Drag vertical splitter right by +84px
    const vBox = (await vSplitter.boundingBox())!;
    await page.mouse.move(vBox.x + vBox.width / 2, vBox.y + vBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(vBox.x + vBox.width / 2 + 84, vBox.y + vBox.height / 2, { steps: 6 });
    await page.mouse.up();

    const w1 = (await leftPane.boundingBox())!.width;
    expect(w1).toBeGreaterThan(w0 + 55);

    // 2. Drag horizontal splitter up by -68px (growing bottom dock)
    const hBox = (await hSplitter.boundingBox())!;
    await page.mouse.move(hBox.x + hBox.width / 2, hBox.y + hBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(hBox.x + hBox.width / 2, hBox.y + hBox.height / 2 - 68, { steps: 6 });
    await page.mouse.up();

    const h1 = (await bottomPane.boundingBox())!.height;
    expect(h1).toBeGreaterThan(h0 + 45);

    // 3. Verify localStorage persistence survives reload
    const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("informal-layout") || "{}"));
    expect(saved.leftW).toBeGreaterThan(280);
    expect(saved.bottomH).toBeGreaterThan(290);

    await page.reload();
    await expect(leftPane).toBeVisible();
    expect((await leftPane.boundingBox())!.width).toBeCloseTo(w1, -1);
    expect((await bottomPane.boundingBox())!.height).toBeCloseTo(h1, -1);

    // 4. Double-click resets both splitters to defaults
    await vSplitter.dblclick();
    await hSplitter.dblclick();
    const resetLayout = await page.evaluate(() => JSON.parse(localStorage.getItem("informal-layout") || "{}"));
    expect(resetLayout.leftW).toBeNull();
    expect(resetLayout.bottomH).toBe(250);

    // 5. Switch to CEX Debug Dockview preset -> multi-pane tabs visible with zero overflow
    await page.getByRole("combobox", { name: "Layout" }).selectOption("cex-debug");
    const dock = page.getByTestId("dockview-workspace");
    await expect(dock).toBeVisible();
    await expect(dock).toContainText("RTL Source");
    await expect(dock).toContainText("Causal Schematic");
    await expect(dock).toContainText("Waveform Trace");

    const overflow = await page.evaluate(() => ({
      scrollW: document.documentElement.scrollWidth,
      clientW: document.documentElement.clientWidth,
    }));
    expect(overflow.scrollW).toBeLessThanOrEqual(overflow.clientW);
  });

  test("Keyboard focus restores cleanly on Escape for every overlay and full workbench passes axe-core in Light & Dark themes", async ({
    page,
  }) => {
    await proveDemo(page);

    // 1. Command Palette (`Ctrl+K`) -> Escape restores focus to `#prop-filter`
    const filter = page.locator("#prop-filter");
    await filter.focus();
    await expect(filter).toBeFocused();
    await page.keyboard.press("Control+k");
    const palette = page.getByRole("dialog", { name: "Command palette" });
    await expect(palette).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(palette).toHaveCount(0);
    await expect(filter).toBeFocused();

    // 2. Shortcut Sheet (`?`) -> Escape restores focus to triggering button (`Prove all`)
    const proveBtn = page.getByRole("button", { name: "Prove all" });
    await proveBtn.focus();
    await expect(proveBtn).toBeFocused();
    await page.keyboard.press("?");
    const sheet = page.getByRole("dialog", { name: /Keyboard & Formal Workflow Guide/i });
    await expect(sheet).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);
    await expect(proveBtn).toBeFocused();

    // 3. Proof budget popover -> click inside -> Escape restores focus to `Proof budget` button
    const budgetBtn = page.getByRole("button", { name: "Proof budget" });
    await budgetBtn.click();
    const pop = page.getByRole("dialog", { name: "Proof budget settings" });
    await expect(pop).toBeVisible();
    await pop.getByRole("button", { name: "5m", exact: true }).click();
    await page.keyboard.press("Escape");
    await expect(pop).toHaveCount(0);
    await expect(budgetBtn).toBeFocused();

    // 4. Property row right-click context menu -> Escape closes menu cleanly
    await page.locator(".table-wrap table.grid tbody tr", { hasText: "a_never9" }).click({ button: "right" });
    const menu = page.getByRole("menu", { name: /Actions for a_never9/i });
    await expect(menu).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(menu).toHaveCount(0);

    // 5. Full-document axe-core accessibility audit in Light & Dark themes (zero serious/critical violations)
    const lightAudit = await new AxeBuilder({ page }).analyze();
    expect(lightAudit.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);

    await switchTheme(page, "Theme: Dark");
    await expect(page.locator(":root")).toHaveAttribute("data-theme", "dark");
    const darkAudit = await new AxeBuilder({ page }).analyze();
    expect(darkAudit.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  });
});
