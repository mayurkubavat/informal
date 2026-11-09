import { useEffect, useState } from "react";
import type { Theme } from "./model";

// "system" leaves data-theme unset so prefers-color-scheme decides.
export function useTheme(): [Theme, (t: Theme) => void] {
  const [theme, setTheme] = useState<Theme>(() => {
    try {
      return (localStorage.getItem("informal-theme") as Theme) || "system";
    } catch {
      return "system";
    }
  });
  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    try {
      localStorage.setItem("informal-theme", theme);
    } catch {
      /* storage unavailable */
    }
  }, [theme]);
  return [theme, setTheme];
}
