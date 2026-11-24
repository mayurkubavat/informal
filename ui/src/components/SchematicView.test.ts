import { describe, expect, it } from "vitest";
import { mockSchematic } from "../mock";
import { computeElkLayout, renderSchematicSvg } from "./SchematicView";

describe("SchematicView ELK layout & themed SVG export", () => {
  it("routes bottom feedback wires with >= 16px clearance below node value labels", async () => {
    const graph = mockSchematic("a_never9", 2);
    const laid = await computeElkLayout(graph);
    expect(laid.nodes.length).toBeGreaterThan(0);

    // Every horizontal edge segment passing beneath a node must clear its value label (n.y + n.h + 12) by >= 16px.
    for (const e of laid.edges) {
      for (let i = 0; i + 1 < e.points.length; i++) {
        const a = e.points[i];
        const b = e.points[i + 1];
        if (Math.abs(a.y - b.y) > 0.5) continue;
        const y = a.y;
        const minX = Math.min(a.x, b.x);
        const maxX = Math.max(a.x, b.x);
        for (const n of laid.nodes) {
          if (maxX <= n.x + 1 || minX >= n.x + n.w - 1) continue;
          if (y > n.y + n.h && y < n.y + n.h + 30) {
            throw new Error(
              `edge ${e.id} horizontal segment at y=${y} collides with value label below node ${n.id} (bottom=${n.y + n.h})`,
            );
          }
        }
      }
    }
  });

  it("renders light and dark themed schematic SVGs", async () => {
    const laid = await computeElkLayout(mockSchematic("a_never9", 2));
    const vals = { "counter.q": "4'h9" };
    const lightSvg = renderSchematicSvg(laid, vals, false);
    const darkSvg = renderSchematicSvg(laid, vals, true);
    expect(lightSvg).toContain('fill="#fdfcfa"');
    expect(lightSvg).toContain("4&#39;h9".replace("&#39;", "'"));
    expect(darkSvg).toContain('fill="#1b1b19"');
    expect(darkSvg).toContain('fill="#e6e3dc"');
    expect(darkSvg).not.toContain('fill="#fdfcfa"');
  });

  it("renders UPF power_domain badges on schematic nodes when present", async () => {
    const graph = mockSchematic("a_never9", 2);
    graph.nodes[0] = { ...graph.nodes[0], power_domain: "PD_CORE" };
    const laid = await computeElkLayout(graph);
    const svg = renderSchematicSvg(laid, {}, false);
    expect(svg).toContain("PD_CORE");
  });
});
