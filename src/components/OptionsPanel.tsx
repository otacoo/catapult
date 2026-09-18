import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl, openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ExternalLink,
  FolderOpen,
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
import { playNotificationSound } from "../utils/sounds";
import AppUpdatesCard from "./AppUpdatesCard";
import AppearanceCard from "./AppearanceCard";
import { setQuickBenchEnabled } from "../utils/appSettings";

type Section = "general" | "chat" | "appearance" | "about";

const SECTIONS: { id: Section; label: string; icon: LucideIcon }[] = [
  { id: "general", label: "General", icon: SlidersHorizontal },
  { id: "chat", label: "Chat", icon: MessageSquare },
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "about", label: "About", icon: Info },
];

function RolePickers({ appConfig, onSet, onSetParams, subagentsEnabled }: {
  appConfig: AppConfig | null;
  onSet: (orchestrator: string | null, worker: string | null) => void;
  onSetParams: (role: "orchestrator" | "worker", ctxSize: number | null, nGpuLayers: number | null) => void;
  subagentsEnabled: boolean;
}) {
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [vramMb, setVramMb] = useState<number | null>(null);

  useEffect(() => {
    invoke<ModelInfo[]>("list_installed_models").then(setModels).catch(() => {});
    invoke<{ gpus: { vram_mb: number }[] }>("get_system_info")
      .then((s) => setVramMb(s.gpus.reduce((a, g) => a + (g.vram_mb || 0), 0)))
      .catch(() => {});
  }, []);

  const fmtGB = (bytes: number) => `${(bytes / 1073741824).toFixed(1)} GB`;
  const sizeOf = (path: string | null | undefined) =>
    path ? models?.find((m) => m.path === path)?.size_bytes ?? null : null;

  const orchPath = appConfig?.harness_roles?.orchestrator ?? null;
  const workerPath = appConfig?.harness_roles?.worker ?? null;
  const orchSize = sizeOf(orchPath);
  // An unset worker runs on the orchestrator model (single-model harness).
  const workerSize = workerPath ? sizeOf(workerPath) : orchSize;
  const combined = (orchSize ?? 0) + (workerPath && workerPath !== orchPath ? (workerSize ?? 0) : 0);
  const overVram = vramMb !== null && vramMb > 0 && combined > vramMb * 1048576;

  const modelOptions = (models ?? []).map((m) => ({ value: m.path, label: m.name }));
  const roleParams = appConfig?.harness_role_params;

  const renderTuning = (
    role: "orchestrator" | "worker",
    path: string | null,
    sizeBytes: number | null,
  ) => {
    if (!path) return null;
    const p = role === "worker" ? roleParams?.worker : roleParams?.orchestrator;
    const num = (v: string) => (v === "" ? null : Math.max(0, parseInt(v, 10) || 0));
    return (
      <div className="mt-1.5 space-y-1.5">
        <p className="text-[11px] text-gray-600">
          {sizeBytes !== null ? `≈ ${fmtGB(sizeBytes)} on disk` : "size unknown (not installed)"}
        </p>
        <div className="flex items-center gap-2">
          <label className="flex items-center gap-1 text-[11px] text-gray-500">
            <span>Ctx</span>
            <input
              type="number"
              min={0}
              step={1024}
              placeholder="auto"
              title="Context size override for this role (empty = auto)"
              className="input w-20 py-0.5 px-1.5 text-[11px]"
              value={p?.ctx_size ?? ""}
              onChange={(e) => {
                const cur = role === "worker" ? roleParams?.worker : roleParams?.orchestrator;
                onSetParams(role, num(e.target.value), cur?.n_gpu_layers ?? null);
              }}
            />
          </label>
          <label className="flex items-center gap-1 text-[11px] text-gray-500">
            <span>GPU layers</span>
            <input
              type="number"
              min={0}
              placeholder="auto"
              title="GPU layers override for this role (empty = auto)"
              className="input w-16 py-0.5 px-1.5 text-[11px]"
              value={p?.n_gpu_layers ?? ""}
              onChange={(e) => {
                const cur = role === "worker" ? roleParams?.worker : roleParams?.orchestrator;
                onSetParams(role, cur?.ctx_size ?? null, num(e.target.value));
              }}
            />
          </label>
        </div>
      </div>
    );
  };

  const sizesKnown =
    (!orchPath || orchSize !== null) && (!workerPath || workerSize !== null);
  const singleModel = !workerPath || workerPath === orchPath;

  return (
    <div className="space-y-2 mt-4">
      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="flex flex-col gap-1 text-xs text-gray-400">
            <span>Orchestrator model (planning)</span>
            <select
              className="input py-1 px-2 text-xs"
              value={orchPath ?? ""}
              onChange={(e) => onSet(e.target.value || null, workerPath)}
            >
              {[{ value: "", label: "Server default" }, ...modelOptions].map((o) => (
                <option key={o.value || "default"} value={o.value}>{o.label}</option>
              ))}
            </select>
          </label>
          {renderTuning("orchestrator", orchPath, orchSize)}
        </div>
        <div>
          <label className={`flex flex-col gap-1 text-xs ${subagentsEnabled ? "text-gray-400" : "text-gray-600"}`}>
            <span>Worker model (subagents)</span>
            <select
              className="input py-1 px-2 text-xs disabled:opacity-50 disabled:cursor-not-allowed"
              value={workerPath ?? ""}
              disabled={!subagentsEnabled}
              onChange={(e) => onSet(orchPath, e.target.value || null)}
            >
              {[{ value: "", label: "Same as orchestrator" }, ...modelOptions].map((o) => (
                <option key={o.value || "default"} value={o.value}>{o.label}</option>
              ))}
            </select>
          </label>
          {subagentsEnabled && renderTuning("worker", workerPath, workerSize)}
        </div>
      </div>
      {sizesKnown && (orchPath || workerPath) && vramMb !== null && vramMb > 0 && (
        <p className={`text-[11px] leading-snug ${overVram ? "text-accent-yellow" : "text-gray-600"}`}>
          {singleModel
            ? `Single model ≈ ${fmtGB(combined)} of ${(vramMb / 1024).toFixed(1)} GB VRAM`
            : `Two models ≈ ${fmtGB(combined)} of ${(vramMb / 1024).toFixed(1)} GB VRAM (needs router mode)`}
          {overVram && " — exceeds VRAM, the router will swap models while switching roles"}
        </p>
      )}
      <p className="text-[11px] text-gray-600 leading-snug">
        A distinct worker model requires router mode: launch on the Run page with no single model selected.
        The orchestrator model loads at run start; a small worker alongside a big planner speeds up execution.
      </p>
    </div>
  );
}

function MemoryCard() {
  interface MemoryFile {
    scope: string;
    path: string;
    exists: boolean;
    text: string;
  }
  const [files, setFiles] = useState<MemoryFile[]>([]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [dirty, setDirty] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    for (const scope of ["global", "project"]) {
      invoke<MemoryFile>("harness_memory_get", { scope })
        .then((f) => {
          setFiles((prev) => [...prev.filter((x) => x.scope !== scope), f]);
          setDrafts((d) => (d[scope] === undefined ? { ...d, [scope]: f.text } : d));
        })
        .catch(() => {});
    }
  };

  useEffect(load, []);

  const save = async (scope: string) => {
    setError(null);
    try {
      await invoke("harness_memory_set", { scope, text: drafts[scope] ?? "" });
      setDirty((d) => ({ ...d, [scope]: false }));
      load();
    } catch (e) {
      setError(String(e));
    }
  };

  const editor = (scope: string, title: string, hint: string) => {
    const f = files.find((x) => x.scope === scope);
    if (scope === "project" && !f) return null;
    return (
      <div className="border border-border p-3">
        <div className="flex items-baseline justify-between gap-2">
          <div className="min-w-0">
            <p className="text-xs font-medium text-gray-300">{title}</p>
            <p className="text-[10px] text-gray-600 truncate font-mono">{f?.path ?? ""}</p>
          </div>
          <div className="flex items-center gap-1 shrink-0">
            {f?.path && (
              <button
                className="btn-ghost text-[10px] py-0.5 px-1.5"
                title="Reveal file in Explorer"
                onClick={async () => {
                  try { await revealItemInDir(f.path); } catch {}
                }}
              >
                <FolderOpen size={11} />
              </button>
            )}
            <button
              className="btn-primary text-[10px] py-0.5 px-2"
              disabled={!dirty[scope]}
              onClick={() => save(scope)}
            >
              Save
            </button>
          </div>
        </div>
        <p className="text-[10px] text-gray-600 mt-1">{hint}</p>
        <textarea
          className="input w-full mt-1.5 font-mono text-[11px] leading-snug"
          rows={5}
          placeholder="Empty — facts the agent saves with its remember tool land here."
          value={drafts[scope] ?? ""}
          onChange={(e) => {
            setDrafts((d) => ({ ...d, [scope]: e.target.value }));
            setDirty((d) => ({ ...d, [scope]: true }));
          }}
        />
      </div>
    );
  };

  return (
    <div className="card">
      <h2 className="section-title mb-1">Memory</h2>
      <p className="section-desc">
        What the agent remembers across sessions. Injected into its system prompt;
        the agent curates it via the remember tool (writes need your approval).
      </p>
      <div className="space-y-3 mt-3">
        {editor("global", "Global memory (MEMORY.md)", "Applies to every project — facts about you and your preferences.")}
        {editor("project", "Project memory (MEMORY.md)", "Applies to the active project only — conventions and corrections.")}
      </div>
      {error && <p className="text-xs text-accent-red mt-2">{error}</p>}
    </div>
  );
}

function SkillsCard() {
  const [skills, setSkills] = useState<{ name: string; description: string; scope: string; dir: string }[] | null>(null);

  const load = () => {
    invoke<{ name: string; description: string; scope: string; dir: string }[]>("harness_skills_list")
      .then(setSkills)
      .catch(() => setSkills([]));
  };

  useEffect(load, []);

  return (
    <div className="card">
      <h2 className="section-title mb-1">Skills</h2>
      <p className="section-desc">
        Procedures the agent has learned. Each skill is a folder with a SKILL.md
        (name + description + instructions); only names/descriptions reach the
        system prompt — the agent loads the rest via its skill tool.
      </p>
      <div className="flex items-center gap-1 mt-3">
        <button
          className="btn-ghost text-[10px] py-0.5 px-1.5"
          title="Open global skills folder"
          onClick={async () => {
            try {
              const mem = await invoke<{ path: string }>("harness_memory_get", { scope: "global" });
              const base = mem.path.split(/[\\/]/).slice(0, -1).join("/");
              await openPath(`${base}/skills`);
            } catch {}
          }}
        >
          <FolderOpen size={11} /> Global folder
        </button>
        <button
          className="btn-ghost text-[10px] py-0.5 px-1.5"
          title="Refresh list"
          onClick={load}
        >
          <RefreshCw size={11} /> Refresh
        </button>
      </div>
      {skills && skills.length === 0 && (
        <p className="text-[11px] text-gray-500 mt-2">No skills yet — ask the agent to save one (manage_skill).</p>
      )}
      {skills && skills.length > 0 && (
        <div className="space-y-1 mt-2">
          {skills.map((s) => (
            <div key={`${s.scope}:${s.name}`} className="flex items-center gap-2 border border-border px-2.5 py-1.5">
              <span className={`badge-${s.scope === "project" ? "blue" : "purple"} text-[9px] shrink-0`}>
                {s.scope}
              </span>
              <div className="flex-1 min-w-0">
                <p className="text-xs text-gray-200 truncate">{s.name}</p>
                {s.description && <p className="text-[10px] text-gray-500 truncate">{s.description}</p>}
              </div>
              <button
                className="btn-ghost text-[10px] py-0.5 px-1.5 shrink-0"
                title="Open skill folder"
                onClick={async () => {
                  try { await openPath(s.dir); } catch {}
                }}
              >
                <FolderOpen size={11} />
              </button>
            </div>
          ))}
        </div>
      )}
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
      invoke<string>("get_harness_system_prompt_default").then(setBuiltInPrompt).catch(() => {});
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
    // The Run tab shows the planned models — refresh it immediately.
    window.dispatchEvent(new CustomEvent("catapult-settings"));
  };

  const setSubagentsEnabled = async (enabled: boolean) => {
    setAppConfig((c) => (c ? { ...c, harness_subagents_enabled: enabled } : c));
    try {
      await invoke("set_harness_subagents_enabled", { enabled });
    } catch {}
  };

  const setRoleParams = async (role: "orchestrator" | "worker", ctxSize: number | null, nGpuLayers: number | null) => {
    const next = { ctx_size: ctxSize, n_gpu_layers: nGpuLayers };
    setAppConfig((c) =>
      c
        ? {
            ...c,
            harness_role_params: role === "worker"
              ? { ...c.harness_role_params, worker: next }
              : { ...c.harness_role_params, orchestrator: next },
          }
        : c,
    );
    try {
      await invoke("set_harness_role_params", { role, ctx_size: ctxSize, n_gpu_layers: nGpuLayers });
    } catch {}
    window.dispatchEvent(new CustomEvent("catapult-settings"));
  };

  // Local draft so typing doesn't hammer config writes; null = pristine.
  const [promptDraft, setPromptDraft] = useState<string | null>(null);
  const [builtInPrompt, setBuiltInPrompt] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setPromptDraft(null);
    }
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

  const setSound = async (key: "agent" | "permissions" | "errors", v: boolean) => {
    setAppConfig((c) =>
      !c
        ? c
        : key === "agent"
          ? { ...c, sound_agent: v }
          : key === "permissions"
            ? { ...c, sound_permissions: v }
            : { ...c, sound_errors: v },
    );
    try {
      await invoke(key === "agent" ? "set_sound_agent" : key === "permissions" ? "set_sound_permissions" : "set_sound_errors", {
        enabled: v,
      });
    } catch {}
    // Preview when enabling (the click is a user gesture, so playback works).
    if (v) void playNotificationSound(key);
  };

  const notificationsCard = (
    <div className="card">
      <h2 className="section-title mb-1">Notifications</h2>
      <p className="section-desc">Sound effects for agent activity.</p>
      <div className="mt-3 divide-y divide-border/60">
        <div className="py-2.5 first:pt-0 last:pb-0">
          <Toggle
            label="Agent"
            hint="Play a sound when the agent finishes a prompt."
            checked={appConfig?.sound_agent ?? true}
            onChange={(v) => setSound("agent", v)}
          />
        </div>
        <div className="py-2.5 first:pt-0 last:pb-0">
          <Toggle
            label="Permissions"
            hint="Play a sound when the agent needs your attention (permission grant)."
            checked={appConfig?.sound_permissions ?? true}
            onChange={(v) => setSound("permissions", v)}
          />
        </div>
        <div className="py-2.5 first:pt-0 last:pb-0">
          <Toggle
            label="Errors"
            hint="Play a sound when an error occurs."
            checked={appConfig?.sound_errors ?? true}
            onChange={(v) => setSound("errors", v)}
          />
        </div>
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
      <div className="space-y-3 mt-3">
        <Toggle
          label="Enable subagents"
          hint="Off runs the orchestrator alone on a single model (WebUI-style chat with tools)."
          checked={appConfig?.harness_subagents_enabled ?? true}
          onChange={setSubagentsEnabled}
        />
      </div>
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
      <RolePickers appConfig={appConfig} onSet={setRoles} onSetParams={setRoleParams} subagentsEnabled={appConfig?.harness_subagents_enabled ?? true} />
    </div>
  );

  const storedPrompt = appConfig?.harness_system_prompt ?? "";
  const shownBase = storedPrompt !== "" ? storedPrompt : builtInPrompt ?? "";
  const promptDirty =
    promptDraft !== null && promptDraft.trim() !== shownBase.trim();

  const systemPromptCard = (
    <div className="card">
      <h2 className="section-title mb-1">System prompt</h2>
      <p className="section-desc">
        Showing the built-in default — edit to override it. Clearing the text and
        saving restores the default.
        The project directory line is always appended, so sandbox awareness survives customization.
      </p>
      <textarea
        className="input w-full mt-3 font-mono text-xs leading-relaxed"
        rows={8}
        placeholder="Loading built-in default…"
        value={promptDraft ?? shownBase}
        onChange={(e) => setPromptDraft(e.target.value)}
      />
      <div className="flex items-center gap-2 mt-2">
        <button
          className="btn-primary text-xs"
          disabled={!promptDirty}
          onClick={() => {
            const v = (promptDraft ?? "").trim();
            // Saving the default verbatim (or empty) stores no override.
            setSystemPrompt(v === "" || v === (builtInPrompt ?? "").trim() ? null : v);
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

      <div className="flex-1 overflow-y-auto p-6">
        <div className={section === "chat" ? "space-y-4" : "max-w-3xl space-y-4"}>
          {section === "general" && (
            <>
              {generalCard}
              {notificationsCard}
              <AppUpdatesCard />
            </>
          )}
          {section === "chat" && (
            <>
              {chatEngineCard}
              <hr className="border-border" />
              {/* Harness-specific options are inert while the harness is off. */}
              <div className={`grid grid-cols-2 gap-4 items-start ${appConfig?.harness_chat === false ? "opacity-50 pointer-events-none" : ""}`}>
                <div className="space-y-4">
                  {agentCard}
                  {systemPromptCard}
                  <MemoryCard />
                </div>
                <div className="space-y-4">
                  <SkillsCard />
                </div>
              </div>
            </>
          )}
          {section === "appearance" && <AppearanceCard />}
          {section === "about" && aboutCard}
        </div>
      </div>
    </div>
  );
}
