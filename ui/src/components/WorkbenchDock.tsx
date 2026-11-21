// Multi-Pane Docking Workspace (`dockview` `createDockview` + React portals)
// for simultaneous side-by-side inspection:
// - `cex-debug`: CodeMirror RTL Source + HiDPI Canvas Trace + ELK.js Causal Schematic
// - `split`: Property Table / Source on left + Trace / Schematic on right
// - `signoff`: Property Table + Formal Apps & Signoff side-by-side

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { createDockview, type DockviewApi, type GroupPanelPartInitParameters, type IContentRenderer, type ITabRenderer } from "dockview";
import "dockview/dist/styles/dockview.css";
import type { LayoutPreset } from "../store/workbenchStore";

export interface DockSlots {
  table: React.ReactNode;
  source: React.ReactNode;
  trace: React.ReactNode;
  schematic: React.ReactNode;
  apps: React.ReactNode;
}

interface MountedPanel {
  id: string;
  slot: keyof DockSlots;
  element: HTMLElement;
}

function applyPresetLayout(api: DockviewApi, preset: Exclude<LayoutPreset, "triage">) {
  api.clear();
  if (preset === "cex-debug") {
    const pSource = api.addPanel({
      id: "panel-source",
      component: "slot",
      title: "RTL Source (Live Values)",
      params: { slot: "source" },
    });
    const pSchem = api.addPanel({
      id: "panel-schematic",
      component: "slot",
      title: "Causal Schematic",
      params: { slot: "schematic" },
      position: { referencePanel: pSource, direction: "right" },
    });
    api.addPanel({
      id: "panel-trace",
      component: "slot",
      title: "Waveform Trace",
      params: { slot: "trace" },
      position: { referencePanel: pSource, direction: "below" },
    });
    api.addPanel({
      id: "panel-table",
      component: "slot",
      title: "Properties",
      params: { slot: "table" },
      position: { referencePanel: pSchem, direction: "within" },
    });
    pSchem.api.setActive();
  } else if (preset === "signoff") {
    const pTable = api.addPanel({
      id: "panel-table",
      component: "slot",
      title: "Properties",
      params: { slot: "table" },
    });
    api.addPanel({
      id: "panel-apps",
      component: "slot",
      title: "Formal Apps & Signoff",
      params: { slot: "apps" },
      position: { referencePanel: pTable, direction: "right" },
    });
  } else {
    const pLeft = api.addPanel({
      id: "panel-table",
      component: "slot",
      title: "Properties",
      params: { slot: "table" },
    });
    api.addPanel({
      id: "panel-source",
      component: "slot",
      title: "RTL Source",
      params: { slot: "source" },
      position: { referencePanel: pLeft, direction: "within" },
    });
    const pRight = api.addPanel({
      id: "panel-trace",
      component: "slot",
      title: "Waveform Trace",
      params: { slot: "trace" },
      position: { referencePanel: pLeft, direction: "right" },
    });
    api.addPanel({
      id: "panel-schematic",
      component: "slot",
      title: "Causal Schematic",
      params: { slot: "schematic" },
      position: { referencePanel: pRight, direction: "within" },
    });
    pRight.api.setActive();
  }
}

export function WorkbenchDock({
  preset,
  slots,
  dark,
}: {
  preset: Exclude<LayoutPreset, "triage">;
  slots: DockSlots;
  dark: boolean;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const apiRef = useRef<DockviewApi | null>(null);
  const appliedPreset = useRef<string | null>(null);
  const [panels, setPanels] = useState<MountedPanel[]>([]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const api = createDockview(host, {
      defaultTabComponent: "clean",
      createTabComponent: (): ITabRenderer => {
        const el = document.createElement("span");
        el.className = "dv-clean-tab-label";
        return {
          element: el,
          init(parameters) {
            el.textContent = parameters.title;
          },
        };
      },
      createComponent: (opts): IContentRenderer => {
        const el = document.createElement("div");
        el.className = "dock-slot-panel";
        let slotKey: keyof DockSlots = "table";
        return {
          element: el,
          init(parameters: GroupPanelPartInitParameters) {
            slotKey = ((parameters.params as { slot?: keyof DockSlots })?.slot) ?? "table";
            setPanels((prev) => [...prev.filter((p) => p.id !== opts.id), { id: opts.id, slot: slotKey, element: el }]);
          },
          dispose() {
            setPanels((prev) => prev.filter((p) => p.id !== opts.id));
          },
        };
      },
    });
    apiRef.current = api;
    appliedPreset.current = preset;
    applyPresetLayout(api, preset);

    return () => {
      api.dispose();
      apiRef.current = null;
      setPanels([]);
    };
  }, []);

  useEffect(() => {
    if (apiRef.current && appliedPreset.current !== preset) {
      appliedPreset.current = preset;
      applyPresetLayout(apiRef.current, preset);
    }
  }, [preset]);

  return (
    <>
      <div
        ref={hostRef}
        className={`workbench-dockview ${dark ? "dockview-theme-abyss" : "dockview-theme-light"}`}
        data-testid="dockview-workspace"
        data-preset={preset}
      />
      {panels.map((p) => createPortal(slots[p.slot], p.element, p.id))}
    </>
  );
}
