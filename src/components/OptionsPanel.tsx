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

  const setHarnessChat = async (enabled: boolean) => {
    setAppConfig((c) => (c ? { ...c, harness_chat: enabled } : c));
    try {
      await invoke("set_harness_chat", { enabled });
    } catch {}
  };

  const setMaxTurns = async (orchestrator: number | null, subagent: number | null) => {
    const nextOrch = orchestrator ?? appConfig?.harness_max_turns ?? 40;
    const nextSub = subagent ?? appConfig?.harness_subagent_max_turns ?? 25;
    setAppConfig((c) =>
      c
        ? {
            ...c,
            harness_max_turns: Math.max(1, Math.min(500, nextOrch)),
            harness_subagent_max_turns: Math.max(1, Math.min(200, nextSub)),
          }
        : c,
    );
    try {
      await invoke("set_harness_max_turns", {
        orchestrator: Math.max(1, Math.min(500, nextOrch)),
        subagent: Math.max(1, Math.min(200, nextSub)),
      });
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
            hint="Closing hides Catapult to the tray instead of quitting."
            checked={appConfig?.close_to_tray ?? false}
            onChange={setCloseToTray}
          />
          <Toggle
            label="Enable Quick Bench"
            hint="Toggle benchmarking tools."
            checked={appConfig?.enable_quick_bench ?? true}
            onChange={setQuickBench}
          />
          <Toggle
            label="Use Catapult Chat"
            hint="Agent harness chat; off uses the llama-server WebUI."
            checked={appConfig?.harness_chat ?? true}
            onChange={setHarnessChat}
          />
          <div className="grid grid-cols-2 gap-3">
            <label className="flex items-center justify-between gap-2 text-xs text-gray-400">
              <span>Agent max turns</span>
              <input
                type="number"
                min={1}
                max={500}
                className="input w-20 py-1 px-2 text-xs"
                value={appConfig?.harness_max_turns ?? 40}
                onChange={(e) => setMaxTurns(parseInt(e.target.value || "40", 10), null)}
              />
            </label>
            <label className="flex items-center justify-between gap-2 text-xs text-gray-400">
              <span>Subagent turns</span>
              <input
                type="number"
                min={1}
                max={200}
                className="input w-20 py-1 px-2 text-xs"
                value={appConfig?.harness_subagent_max_turns ?? 25}
                onChange={(e) => setMaxTurns(null, parseInt(e.target.value || "25", 10))}
              />
            </label>
          </div>
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
