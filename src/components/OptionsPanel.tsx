import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  ExternalLink,
  Info,
  MessageSquare,
  Palette,
  RefreshCw,
  SlidersHorizontal,
  X,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { AppConfig, ModelInfo } from "../types";
import Toggle from "./Toggle";
import AppUpdatesCard from "./AppUpdatesCard";
import AppearanceCard from "./AppearanceCard";
import { setQuickBenchEnabled } from "../utils/appSettings";

type Section = "general" | "chat" | "updates" | "appearance" | "about";

const SECTIONS: { id: Section; label: string; icon: LucideIcon }[] = [
  { id: "general", label: "General", icon: SlidersHorizontal },
  { id: "chat", label: "Chat", icon: MessageSquare },
  { id: "updates", label: "App Updates", icon: RefreshCw },
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "about", label: "About", icon: Info },
];

function RolePickers({ appConfig, onSet }: {
  appConfig: AppConfig | null;
  onSet: (orchestrator: string | null, worker: string | null) => void;
}) {
  const [models, setModels] = useState<ModelInfo[] | null>(null);

  useEffect(() => {
    invoke<ModelInfo[]>("list_installed_models").then(setModels).catch(() => {});
  }, []);

  const options = [
    { value: "", label: "Server default" },
    ...(models ?? []).map((m) => ({ value: m.path, label: m.name })),
  ];

  return (
    <div className="space-y-2 mt-4">
      <div className="grid grid-cols-2 gap-3">
        <label className="flex flex-col gap-1 text-xs text-gray-400">
          <span>Orchestrator model (planning)</span>
          <select
            className="input py-1 px-2 text-xs"
            value={appConfig?.harness_roles?.orchestrator ?? ""}
            onChange={(e) => onSet(e.target.value || null, appConfig?.harness_roles?.worker ?? null)}
          >
            {options.map((o) => (
              <option key={o.value || "default"} value={o.value}>{o.label}</option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1 text-xs text-gray-400">
          <span>Worker model (subagents)</span>
          <select
            className="input py-1 px-2 text-xs"
            value={appConfig?.harness_roles?.worker ?? ""}
            onChange={(e) => onSet(appConfig?.harness_roles?.orchestrator ?? null, e.target.value || null)}
          >
            {options.map((o) => (
              <option key={o.value || "default"} value={o.value}>{o.label}</option>
            ))}
          </select>
        </label>
      </div>
      <p className="text-[11px] text-gray-600 leading-snug">
        Role models require router mode: launch on the Run page with no single model selected
        (pin models with the layers icon). The orchestrator model loads at run start; a small
        worker model alongside a big planner speeds up execution.
      </p>
    </div>
  );
}

export default function OptionsPanel({ open, onClose }: {
  open: boolean;
  onClose: () => void;
}) {
  const [section, setSection] = useState<Section>("general");
  const [appConfig, setAppConfig] = useState<AppConfig | null>(null);

  useEffect(() => {
    if (open) {
      invoke<AppConfig>("get_config").then(setAppConfig).catch(() => {});
    }
  }, [open]);

  // Close with Escape (the gear button also toggles).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
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
    // The Chat tab caches the mode — refresh it immediately.
    window.dispatchEvent(new CustomEvent("catapult-settings"));
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

  const setRoles = async (orchestrator: string | null, worker: string | null) => {
    setAppConfig((c) =>
      c ? { ...c, harness_roles: { orchestrator, worker } } : c,
    );
    try {
      await invoke("set_harness_roles", { orchestrator, worker });
    } catch {}
  };

  // Local draft so typing doesn't hammer config writes; null = pristine.
  const [promptDraft, setPromptDraft] = useState<string | null>(null);
  useEffect(() => {
    if (open) setPromptDraft(null);
  }, [open ]);

  const setSystemPrompt = async (prompt: string | null) => {
    setAppConfig((c) => (c ? { ...c, harness_system_prompt: prompt } : c));
    try {
      await invoke("set_harness_system_prompt", { prompt: prompt ?? "" });
    } catch {}
  };

  const generalCard = (
    <div className="card">
      <h2 className="section-title mb-1">General</h2>
      <p className="section-desc">Core app behavior.</p>
      <div className="space-y-3 mt-3">
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
      </div>
    </div>
  );

  const chatEngineCard = (
    <div className="card">
      <h2 className="section-title mb-1">Catapult Chat</h2>
      <p className="section-desc">Agent harness chat for the Chat tab.</p>
      <div className="space-y-3 mt-3">
        <Toggle
          label="Use Catapult Chat"
          hint="Agent harness chat; off uses the llama-server WebUI."
          checked={appConfig?.harness_chat ?? true}
          onChange={setHarnessChat}
        />
      </div>
    </div>
  );

  const agentCard = (
    <div className="card">
      <h2 className="section-title mb-1">Agent</h2>
      <p className="section-desc">
        Turn budgets and model roles for the Catapult agent harness.
      </p>
      <div className="grid grid-cols-2 gap-3 mt-3">
        <label className="flex items-center justify-between gap-2 text-xs text-gray-400">
          <span>Orchestrator max turns</span>
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
      <RolePickers appConfig={appConfig} onSet={setRoles} />
    </div>
  );

  const storedPrompt = appConfig?.harness_system_prompt ?? "";
  const promptDirty =
    promptDraft !== null && promptDraft.trim() !== storedPrompt.trim();

  const systemPromptCard = (
    <div className="card">
      <h2 className="section-title mb-1">System prompt</h2>
      <p className="section-desc">
        Override the agent's system prompt. Empty restores the built-in default.
        The project directory line is always appended, so sandbox awareness survives customization.
      </p>
      <textarea
        className="input w-full mt-3 font-mono text-xs leading-relaxed"
        rows={8}
        placeholder="Leave empty to use the built-in default…"
        value={promptDraft ?? storedPrompt}
        onChange={(e) => setPromptDraft(e.target.value)}
      />
      <div className="flex items-center gap-2 mt-2">
        <button
          className="btn-primary text-xs"
          disabled={!promptDirty}
          onClick={() => {
            const v = (promptDraft ?? "").trim();
            setSystemPrompt(v === "" ? null : v);
            setPromptDraft(null);
          }}
        >
          Save
        </button>
        {storedPrompt !== "" && (
          <button
            className="btn-secondary text-xs"
            onClick={() => {
              setSystemPrompt(null);
              setPromptDraft(null);
            }}
          >
            Reset to default
          </button>
        )}
        {!promptDirty && storedPrompt !== "" && (
          <span className="text-[11px] text-gray-500">Custom prompt active</span>
        )}
        {!promptDirty && storedPrompt === "" && (
          <span className="text-[11px] text-gray-500">Using built-in default</span>
        )}
      </div>
    </div>
  );

  const aboutCard = (
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
  );

  return (
    <div
      style={{ display: open ? undefined : "none" }}
      className="absolute inset-x-0 bottom-0 top-11 z-40 flex bg-surface-0"
    >
      {/* Category sidebar */}
      <aside className="w-52 shrink-0 border-r border-border bg-surface-1 p-3 overflow-y-auto">
        <div className="flex items-center justify-between px-2 pb-3 pt-1">
          <span className="text-sm font-semibold text-gray-200">Settings</span>
          <button
            className="text-gray-500 hover:text-gray-300 transition-colors"
            onClick={onClose}
            title="Close settings (Esc)"
          >
            <X size={15} />
          </button>
        </div>
        <div className="space-y-0.5">
          {SECTIONS.map(({ id, label, icon: Icon }) => (
            <button
              key={id}
              onClick={() => setSection(id)}
              className={`w-full flex items-center gap-2 px-2.5 py-1.5 rounded text-xs font-medium transition-colors ${
                section === id
                  ? "bg-primary/20 text-primary-light"
                  : "text-gray-400 hover:text-gray-200 hover:bg-primary/10"
              }`}
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>
      </aside>

      {/* Content pane */}
      <div className="flex-1 overflow-y-auto p-6">
        <div className="max-w-3xl space-y-4">
          {section === "general" && generalCard}
          {section === "chat" && (
            <>
              {chatEngineCard}
              {/* Harness-specific options are inert while the harness is off. */}
              <div className={appConfig?.harness_chat === false ? "opacity-50 pointer-events-none" : ""}>
                {agentCard}
                {systemPromptCard}
              </div>
            </>
          )}
          {section === "updates" && <AppUpdatesCard />}
          {section === "appearance" && <AppearanceCard />}
          {section === "about" && aboutCard}
        </div>
      </div>
    </div>
  );
}
