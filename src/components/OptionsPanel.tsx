import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink } from "lucide-react";
import type { AppConfig } from "../types";
import Toggle from "./Toggle";
import AppUpdatesCard from "./AppUpdatesCard";
import AppearanceCard from "./AppearanceCard";
import { setQuickBenchEnabled } from "../utils/appSettings";

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

  const openRepo = async (url: string) => {
    try {
      await openUrl(url);
    } catch {}
  };

  const setQuickBench = async (enabled: boolean) => {
    setAppConfig((c) => (c ? { ...c, enable_quick_bench: enabled } : c));
    setQuickBenchEnabled(enabled);
    try {
      await invoke("set_enable_quick_bench", { enabled });
    } catch {}
  };

  return (
    <div
      ref={panelRef}
      style={{ display: open ? undefined : "none" }}
      className="absolute right-2 top-12 z-50 w-[400px] max-w-[calc(100vw-1rem)] max-h-[calc(100vh-3.5rem)] overflow-y-auto space-y-4 p-2 bg-surface-1 border border-border rounded shadow-xl"
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
          <Toggle
            label="Enable Quick Bench"
            hint="Show the Quick Bench button and card on the Run page, and the Bench tab."
            checked={appConfig?.enable_quick_bench ?? true}
            onChange={setQuickBench}
          />
        </div>
      </div>
      <div className="card">
        <div className="flex justify-center mb-2">
          <span className="text-sm font-semibold text-gray-200 tracking-tight">Catapult</span>
        </div>
        <p className="text-xs text-gray-500 mb-2">
          A llama.cpp launcher, licensed under the{" "}
          <a
            href="#"
            onClick={(e) => { e.preventDefault(); openRepo("https://www.apache.org/licenses/LICENSE-2.0"); }}
            className="text-gray-400 hover:text-gray-200 underline decoration-gray-700 hover:decoration-gray-400"
          >
            Apache License 2.0
          </a>.
        </p>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
          <button
            onClick={() => openRepo("https://github.com/pwilkin/catapult")}
            className="text-gray-400 hover:text-gray-200 inline-flex items-center gap-1"
            title="Original repository by Piotr Wilkin"
          >
            <ExternalLink size={11} />
            pwilkin/catapult
          </button>
          <button
            onClick={() => openRepo("https://github.com/otacoo/catapult")}
            className="text-gray-400 hover:text-gray-200 inline-flex items-center gap-1"
            title="This fork"
          >
            <ExternalLink size={11} />
            otacoo/catapult
          </button>
        </div>
      </div>
    </div>
  );
}
