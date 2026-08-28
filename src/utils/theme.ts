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

/** Map a preference to the concrete appearance to render. */
export function resolvePref(pref: AppTheme): ResolvedTheme {
  if (pref === "light") return "light";
  if (pref === "dark" || pref === "catapult") return "dark";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/** Apply a preference immediately: `data-theme` drives the CSS token palettes. */
export function applyPref(pref: AppTheme): void {
  const el = document.documentElement;
  const resolved = resolvePref(pref);
  el.dataset.theme = pref === "system" ? resolved : pref;
  el.style.colorScheme = resolved;
}

/** Set the preference and persist the live state (used by the UI). */
export function setThemePreference(pref: AppTheme): void {
  currentPref = pref;
  applyPref(pref);
}

/**
 * Boot-time init: apply the stored preference and keep "system" in sync with
 * the OS when it changes while the app is running.
 */
export function initTheme(pref: AppTheme): void {
  currentPref = pref;
  applyPref(pref);
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", () => {
      if (currentPref === "system") applyPref("system");
    });
}