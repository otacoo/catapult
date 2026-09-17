import type { AppTheme } from "../types";

export type ResolvedTheme = "dark" | "light";

export const THEME_OPTIONS: {
  value: AppTheme;
  label: string;
  description: string;
}[] = [
  { value: "system", label: "System", description: "Follow OS light/dark" },
  { value: "dark", label: "Dark", description: "Cool neutral dark" },
  { value: "light", label: "Light", description: "Light surfaces" },
  { value: "catapult", label: "Catapult", description: "Branded charcoal + red" },
];

let currentPref: AppTheme = "catapult";

export function resolvePref(pref: AppTheme): ResolvedTheme {
  if (pref === "light") return "light";
  if (pref === "dark" || pref === "catapult") return "dark";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function applyPref(pref: AppTheme): void {
  const el = document.documentElement;
  const resolved = resolvePref(pref);
  el.dataset.theme = pref === "system" ? resolved : pref;
  el.style.colorScheme = resolved;
}

export function setThemePreference(pref: AppTheme): void {
  currentPref = pref;
  applyPref(pref);
}

/** Boot init: apply stored pref; keep "system" in sync with OS changes. */
export function initTheme(pref: AppTheme): void {
  currentPref = pref;
  applyPref(pref);
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", () => {
      if (currentPref === "system") applyPref("system");
    });
}