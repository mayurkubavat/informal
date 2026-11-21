// Shared active theme palette resolver for deterministic SVG exports (`gui_capture_view`)
// and canvas overlays across light, dark, and MCP App host-themed surfaces.

export interface SvgPalette {
  dark: boolean;
  surface: string;
  surface2: string;
  bg: string;
  chrome: string;
  border: string;
  borderStrong: string;
  text: string;
  text2: string;
  muted: string;
  accent: string;
  hlLine: string;
  stCex: string;
  stCovered: string;
  stUndetermined: string;
  stProven: string;
  stError: string;
  wireMuted: string;
  nodeFill: string;
  causalFill: string;
  hotspotFill: string;
}

export function detectDarkTheme(): boolean {
  if (typeof document === "undefined") return false;
  const pinned = document.documentElement.dataset.theme;
  if (pinned === "dark") return true;
  if (pinned === "light") return false;
  return typeof window !== "undefined" && Boolean(window.matchMedia?.("(prefers-color-scheme: dark)").matches);
}

export function resolveSvgPalette(darkOverride?: boolean): SvgPalette {
  const domDark = detectDarkTheme();
  const dark = darkOverride ?? domDark;
  const cs =
    typeof window !== "undefined" && typeof document !== "undefined" && dark === domDark
      ? window.getComputedStyle(document.documentElement)
      : null;
  const cssVar = (name: string, fallback: string) => {
    const v = cs?.getPropertyValue(name)?.trim();
    return v || fallback;
  };
  if (dark) {
    return {
      dark: true,
      surface: cssVar("--surface", "#1b1b19"),
      surface2: cssVar("--surface-2", "#2a2a27"),
      bg: cssVar("--bg", "#1e1e1c"),
      chrome: cssVar("--chrome", "#232321"),
      border: cssVar("--border", "#353430"),
      borderStrong: cssVar("--border-strong", "#46443e"),
      text: cssVar("--text", "#e6e3dc"),
      text2: cssVar("--text-2", "#c3c0b6"),
      muted: cssVar("--muted", "#9c988d"),
      accent: cssVar("--accent", "#5fb3ab"),
      hlLine: cssVar("--hl-line", "#3a3322"),
      stCex: cssVar("--st-cex", "#f08a5d"),
      stCovered: cssVar("--st-covered", "#8fa8f0"),
      stUndetermined: cssVar("--st-undetermined", "#e0ae4a"),
      stProven: cssVar("--st-proven", "#5dbb82"),
      stError: cssVar("--st-error", "#f2766b"),
      wireMuted: "#78746a",
      nodeFill: "#252522",
      causalFill: "#2f241d",
      hotspotFill: "#332a17",
    };
  }
  return {
    dark: false,
    surface: cssVar("--surface", "#fdfcfa"),
    surface2: cssVar("--surface-2", "#f1efe9"),
    bg: cssVar("--bg", "#f6f5f1"),
    chrome: cssVar("--chrome", "#eeece5"),
    border: cssVar("--border", "#e3e0d7"),
    borderStrong: cssVar("--border-strong", "#d9d5ca"),
    text: cssVar("--text", "#22211e"),
    text2: cssVar("--text-2", "#55524a"),
    muted: cssVar("--muted", "#6a665d"),
    accent: cssVar("--accent", "#1d5f5b"),
    hlLine: cssVar("--hl-line", "#fbf1d9"),
    stCex: cssVar("--st-cex", "#b4471b"),
    stCovered: cssVar("--st-covered", "#3b5bc4"),
    stUndetermined: cssVar("--st-undetermined", "#b45309"),
    stProven: cssVar("--st-proven", "#2f7a4c"),
    stError: cssVar("--st-error", "#b42318"),
    wireMuted: "#8b867a",
    nodeFill: "#f6f5f1",
    causalFill: "#fff7ed",
    hotspotFill: "#fef3c7",
  };
}
