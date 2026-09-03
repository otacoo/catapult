import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppConfig } from "../types";
import Toggle from "./Toggle";
import AppUpdatesCard from "./AppUpdatesCard";
import AppearanceCard from "./AppearanceCard";

export default function OptionsPanel({ open, onClose }: {
  open: boolean;
  onClose: () => void;
}) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const [appConfig, setAppConfig] = useState<AppConfig | null>(null);

  useEffect(() => {
    if (open) {
      invoke<AppConfig>("get_config").then(setAppConfig).catch(() => {});
    }
  }, [open]);

  // Close when clicking outside of the panel
  useEffect(() => {
    if (!open) return;
    const onDocMouseDown = (e: MouseEvent) => {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    // Defer so the gear button's opening click doesn't immediately close it
    const t = setTimeout(() => document.addEventListener("mousedown", onDocMouseDown), 0);
    return () => {
      clearTimeout(t);
      document.removeEventListener("mousedown", onDocMouseDown);
    };
  }, [open, onClose]);

  const setCloseToTray = async (enabled: boolean) => {
    setAppConfig((c) => (c ? { ...c, close_to_tray: enabled } : c));
    try {
      await invoke("set_close_to_tray", { enabled });
    } catch {}
  };

  return (
    <div
      ref={panelRef}
      style={{ display: open ? undefined : "none" }}
      className="absolute right-2 top-12 z-50 w-[400px] max-w-[calc(100vw-1rem)] max-h-[calc(100vh-3.5rem)] overflow-y-auto space-y-4 p-1"
    >
      <AppUpdatesCard />
      <AppearanceCard />
      <div className="card">
        <h2 className="section-title mb-1">General</h2>
        <div className="space-y-3">
          <Toggle
            label="Show in notification area"
            hint="Keep Catapult in the system tray; closing the window hides it there instead of quitting."
            checked={appConfig?.close_to_tray ?? false}
            onChange={setCloseToTray}
          />
        </div>
      </div>
    </div>
  );
}
