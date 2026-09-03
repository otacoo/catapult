import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Monitor, Moon, Sun } from "lucide-react";
import type { AppTheme } from "../types";
import { THEME_OPTIONS, setThemePreference } from "../utils/theme";
import CatapultIcon from "./CatapultIcon";

export default function AppearanceCard() {
  const [theme, setThemeState] = useState<AppTheme>("system");

  useEffect(() => {
    invoke<{ theme: AppTheme }>("get_config")
      .then((c) => setThemeState(c.theme ?? "system"))
      .catch(() => {});
  }, []);

  const handleTheme = async (t: AppTheme) => {
    setThemePreference(t);
    setThemeState(t);
    try {
      await invoke("set_theme", { theme: t });
    } catch {}
  };

  return (
    <div className="card">
      <h2 className="section-title mb-1">Appearance</h2>
      <p className="section-desc">Choose how Catapult looks.</p>
      <div className="grid grid-cols-2 gap-2">
        {THEME_OPTIONS.map((opt) => {
          const active = theme === opt.value;
          const iconCls = active ? "text-primary-light" : "text-gray-500";
          return (
            <button
              key={opt.value}
              onClick={() => handleTheme(opt.value)}
              className={`flex flex-col items-center gap-1.5 px-3 py-3 rounded border text-center transition-colors ${
                active
                  ? "border-primary bg-primary/10 text-gray-200"
                  : "border-border bg-surface-3 hover:bg-surface-4 text-gray-400"
              }`}
            >
              {opt.value === "system" && <Monitor size={18} className={iconCls} />}
              {opt.value === "dark" && <Moon size={18} className={iconCls} />}
              {opt.value === "light" && <Sun size={18} className={iconCls} />}
              {opt.value === "catapult" && (
                <CatapultIcon size={18} className={iconCls} />
              )}
              <span className="text-xs font-medium">{opt.label}</span>
              <span className="text-[10px] text-gray-500 leading-tight">
                {opt.description}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
