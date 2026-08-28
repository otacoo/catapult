import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Pencil, Plus, Trash2, X, Wrench, Globe, AlertTriangle, CheckCircle2, RefreshCw, ExternalLink, FolderOpen } from "lucide-react";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import Toggle from "../components/Toggle";
import { KNOWN_TOOLS, toolsArgValue } from "../utils/tools";
import type { AppConfig, McpInfo, McpServerEntry, ServerStatus, ServerToolInfo } from "../types";

const emptyEntry = (): McpServerEntry => ({
  name: "",
  command: "",
  args: [],
  env: {},
  cwd: null,
  timeout_ms: null,
});

// ── Text ↔ structured conversions for the MCP editor ───────────────────────

const parseArgs = (text: string): string[] =>
  text.split(/\r?\n/).map((s) => s.trim()).filter(Boolean);

const argsToText = (args: string[]): string => args.join("\n");

const parseEnv = (text: string): Record<string, string> => {
  const env: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    const idx = line.indexOf("=");
    if (idx > 0) env[line.slice(0, idx).trim()] = line.slice(idx + 1);
  }
  return env;
};

const envToText = (env: Record<string, string>): string =>
  Object.entries(env).map(([k, v]) => `${k}=${v}`).join("\n");

export default function Tools() {
  const [serverStatus, setServerStatus] = useState<ServerStatus>({ type: "stopped" });
  const [mcp, setMcp] = useState<McpInfo | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const selectedRef = useRef<Set<string>>(new Set());
  const saveTimer = useRef<number | null>(null);

  // MCP editor state
  const [showForm, setShowForm] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [draft, setDraft] = useState<McpServerEntry>(emptyEntry());
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");
  const [error, setError] = useState<string | null>(null);

  // Live server tools probe (what the running server actually advertises at /tools)
  const [liveTools, setLiveTools] = useState<ServerToolInfo[] | null>(null);
  const [liveToolsError, setLiveToolsError] = useState<string | null>(null);
  const [liveToolsLoading, setLiveToolsLoading] = useState(false);

  const fetchLiveTools = useCallback(async (status: ServerStatus) => {
    if (status.type !== "running") {
      setLiveTools(null);
      setLiveToolsError(null);
      return;
    }
    setLiveToolsLoading(true);
    setLiveToolsError(null);
    try {
      const t = await invoke<ServerToolInfo[]>("get_server_tools");
      setLiveTools(t);
    } catch (e) {
      setLiveTools(null);
      setLiveToolsError(String(e));
    } finally {
      setLiveToolsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (serverStatus.type === "running") {
      fetchLiveTools(serverStatus);
      const id = setInterval(() => fetchLiveTools(serverStatus), 5000);
      return () => clearInterval(id);
    } else {
      setLiveTools(null);
      setLiveToolsError(null);
    }
  }, [serverStatus, fetchLiveTools]);

  const load = async () => {
    try {
      const [cfg, mc, srv] = await Promise.all([
        invoke<AppConfig>("get_config"),
        invoke<McpInfo>("list_mcp_servers"),
        invoke<ServerStatus>("get_server_status").catch(() => ({ type: "stopped" as const })),
      ]);
      setMcp(mc);
      setServerStatus(srv);
      setSelected(new Set(cfg.server_tools));
      selectedRef.current = new Set(cfg.server_tools);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    load();
    return () => {
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
    };
  }, []);

  // ── Built-in tools ────────────────────────────────────────────────────

  const persistTools = (sel: Set<string>) => {
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(async () => {
      try {
        await invoke("set_tools", { tools: [...sel].sort() });
      } catch (e) {
        setError(String(e));
      }
    }, 400);
  };

  const toggleTool = (name: string, on: boolean) => {
    if (on && name === "exec_shell_command") {
      if (!window.confirm(
        "Shell Command lets the model execute arbitrary commands on your " +
        "computer with your user's permissions. Continue?")) return;
    }
    const next = new Set(selectedRef.current);
    if (on) next.add(name); else next.delete(name);
    selectedRef.current = next;
    setSelected(next);
    persistTools(next);
  };

  const passing = toolsArgValue([...selected].sort().join(","));

  // ── MCP servers ───────────────────────────────────────────────────────

  const openAdd = () => {
    setEditId(null);
    setDraft(emptyEntry());
    setArgsText("");
    setEnvText("");
    setShowForm(true);
  };

  const openEdit = (e: McpServerEntry) => {
    setEditId(e.name);
    setDraft(e);
    setArgsText(argsToText(e.args));
    setEnvText(envToText(e.env));
    setShowForm(true);
  };

  const closeForm = () => {
    setShowForm(false);
    setEditId(null);
  };

  const saveServers = async (next: McpServerEntry[]) => {
    try {
      await invoke("save_mcp_servers", { servers: next });
      setMcp((m) => (m ? { ...m, servers: next } : m));
      setShowForm(false);
      setEditId(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const saveEntry = async () => {
    const name = draft.name.trim();
    const command = draft.command.trim();
    if (!name || !command) {
      setError("Name and command are required for an MCP server.");
      return;
    }
    setError(null);
    const current = mcp?.servers ?? [];
    const entry: McpServerEntry = {
      ...draft,
      name,
      command,
      args: parseArgs(argsText),
      env: parseEnv(envText),
      cwd: draft.cwd?.trim() || null,
      timeout_ms: draft.timeout_ms,
    };
    const idx = current.findIndex((s) => s.name === name);
    const next = idx >= 0
      ? current.map((s) => (s.name === name ? entry : s))
      : [...current, entry];
    await saveServers(next);
  };

  const removeServer = async (name: string) => {
    if (!window.confirm(`Remove MCP server "${name}"?`)) return;
    setError(null);
    await saveServers((mcp?.servers ?? []).filter((s) => s.name !== name));
  };

  const running = serverStatus.type === "running" || serverStatus.type === "starting";

  return (
    <div className="flex-1 overflow-y-auto p-6 space-y-6">
      {error && (
        <div className="card border-accent-red/30 bg-accent-red/5 text-sm text-accent-red">
          {error}
        </div>
      )}

      {/* Live server tools probe */}
      <div className="card">
        <div className="flex items-center justify-between mb-1">
          <h2 className="section-title mb-0">Live Server Tools</h2>
          <button
            className="btn-ghost text-xs py-1 px-2"
            onClick={() => fetchLiveTools(serverStatus)}
            disabled={liveToolsLoading}
            title="Refresh from /tools"
          >
            <RefreshCw size={12} className={liveToolsLoading ? "animate-spin" : ""} /> Refresh
          </button>
        </div>
        {serverStatus.type !== "running" ? (
          <p className="text-xs text-gray-500 mt-3">Server not running.</p>
        ) : liveToolsLoading && !liveTools ? (
          <p className="text-xs text-gray-500 mt-3 flex items-center gap-2">
            <RefreshCw size={12} className="animate-spin" /> Checking…
          </p>
        ) : liveToolsError ? (
          <p className="text-xs text-accent-yellow mt-3">Could not fetch: <span className="font-mono">{liveToolsError}</span></p>
        ) : liveTools ? (
          (() => {
            const serverTools = liveTools.filter((t) => t.type === "server");
            const mcpTools = liveTools.filter((t) => t.type === "mcp");
            const hasMcp = mcpTools.length > 0;
            return (
              <div className="flex flex-wrap items-center gap-2 text-xs mt-3">
                <span className="flex items-center gap-1.5">
                  <Wrench size={12} className="text-gray-400" />
                  <span className="text-gray-400">Server:</span>
                  <span className="text-gray-200">{serverTools.length}</span>
                </span>
                <span className="text-gray-600">•</span>
                <span className="flex items-center gap-1.5">
                  <Globe size={12} className={hasMcp ? "text-accent-green" : "text-gray-500"} />
                  <span className={hasMcp ? "text-accent-green" : "text-gray-500"}>MCP:</span>
                  <span className={hasMcp ? "text-accent-green font-medium" : "text-gray-500"}>{mcpTools.length}</span>
                  {hasMcp ? <CheckCircle2 size={12} className="text-accent-green" /> : <AlertTriangle size={12} className="text-accent-yellow" />}
                </span>
                {hasMcp && <span className="text-gray-500">({mcpTools.map((t) => t.name).join(", ")})</span>}
              </div>
            );
          })()
        ) : null}
      </div>

      {/* Built-in tools */}
      <div className="card">
        <h2 className="section-title mb-1">Built-in Tools</h2>
        <p className="section-desc">
          File tools llama-server exposes to the model. They apply to every run;
          {running && (
            <span className="text-accent-yellow"> the running server must be restarted to pick up changes.</span>
          )}
        </p>
        <div className="grid grid-cols-2 gap-x-4 gap-y-2.5 mt-3">
          {KNOWN_TOOLS.filter((t) => !t.dangerous).map((t) => (
            <Toggle key={t.name} label={t.label} hint={t.hint}
              checked={selected.has(t.name)}
              onChange={(on) => toggleTool(t.name, on)} />
          ))}
        </div>
        <div className="space-y-2.5 mt-3">
          {KNOWN_TOOLS.filter((t) => t.dangerous).map((t) => (
            <Toggle key={t.name} label={t.label} hint={t.hint}
              checked={selected.has(t.name)}
              onChange={(on) => toggleTool(t.name, on)} />
          ))}
        </div>
        <div className="mt-3 text-xs text-gray-500">
          {passing ? (
            <>
              Passing: <code className="font-mono text-gray-300">--tools {passing}</code>
            </>
          ) : (
            "No tools enabled — file access stays off."
          )}
        </div>
      </div>

      {/* MCP servers */}
      <div className="card">
        <div className="flex items-center justify-between mb-1">
          <h2 className="section-title mb-0">MCP Servers</h2>
          {!showForm && (
            <button className="btn-secondary text-xs py-1 px-2" onClick={openAdd}>
              <Plus size={12} className="inline mr-1" />Add MCP server
            </button>
          )}
        </div>
        <p className="section-desc">MCP servers expose tools to the model.</p>

        {(mcp?.servers.length ?? 0) > 0 ? (
          <div className="space-y-2 mt-3">
            {mcp!.servers.map((s) => (
              <div key={s.name} className="flex items-center gap-3 rounded border border-border bg-surface-2 px-3 py-2">
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-gray-200 font-mono">{s.name}</p>
                  <p className="text-xs text-gray-500 truncate font-mono">{s.command}</p>
                  <p className="text-xs text-gray-600 mt-0.5">
                    {s.args.length > 0 && `${s.args.length} arg(s)`}
                    {s.args.length > 0 && s.env && Object.keys(s.env).length > 0 && " · "}
                    {s.env && Object.keys(s.env).length > 0 && `${Object.keys(s.env).length} env var(s)`}
                    {s.timeout_ms != null && ` · timeout ${s.timeout_ms}ms`}
                  </p>
                </div>
                <button className="btn-ghost text-xs py-1 px-2" onClick={() => openEdit(s)}>
                  <Pencil size={12} className="inline mr-1" />Edit
                </button>
                <button className="btn-ghost text-xs py-1 px-2 text-accent-red" onClick={() => removeServer(s.name)}>
                  <Trash2 size={12} className="inline mr-1" />Remove
                </button>
              </div>
            ))}
          </div>
        ) : (
          <p className="text-sm text-gray-500 mt-3">
            No MCP servers configured. Add one to attach a tool server (e.g. a web fetch/search
            server) to every run.
          </p>
        )}

        {showForm && (
          <div className="rounded border border-border bg-surface-2 p-4 mt-3 space-y-3">
            <div className="flex items-center justify-between">
              <p className="text-sm font-medium text-gray-200">
                {editId ? `Edit "${editId}"` : "Add MCP Server"}
              </p>
              <button className="btn-ghost text-xs px-1.5 py-0.5" onClick={closeForm}>
                <X size={12} />
              </button>
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div>
                <label className="label">Name</label>
                <input type="text" className="input" value={draft.name} placeholder="e.g. fetch"
                  onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))} />
              </div>
              <div>
                <label className="label">Command</label>
                <input type="text" className="input font-mono text-xs" value={draft.command}
                  placeholder="e.g. npx, uvx, .\\server.exe" onChange={(e) => setDraft((d) => ({ ...d, command: e.target.value }))} />
              </div>
            </div>
            <div>
              <label className="label">Args</label>
              <p className="text-xs text-gray-600 mb-1">One argument per line</p>
              <textarea className="input font-mono text-xs min-h-[64px]" value={argsText}
                placeholder={"-y\n@modelcontextprotocol/server-fetch"}
                onChange={(e) => setArgsText(e.target.value)} />
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div>
                <label className="label">Environment</label>
                <p className="text-xs text-gray-600 mb-1">KEY=VALUE, one per line</p>
                <textarea className="input font-mono text-xs min-h-[64px]" value={envText}
                  placeholder={"API_KEY=xyz"}
                  onChange={(e) => setEnvText(e.target.value)} />
              </div>
              <div className="space-y-3">
                <div>
                  <label className="label">Working Directory (optional)</label>
                  <input type="text" className="input font-mono text-xs" value={draft.cwd ?? ""}
                    placeholder="e.g. D:\work" onChange={(e) => setDraft((d) => ({ ...d, cwd: e.target.value }))} />
                </div>
                <div>
                  <label className="label">Timeout (ms, optional)</label>
                  <input type="number" min={0} className="input font-mono text-xs"
                    value={draft.timeout_ms ?? ""} placeholder="default: 30000"
                    onChange={(e) => {
                      const n = e.target.value === "" ? null : Number(e.target.value);
                      setDraft((d) => ({ ...d, timeout_ms: n && !isNaN(n) ? n : null }));
                    }} />
                </div>
              </div>
            </div>
            <div className="flex gap-2">
              <button className="btn-primary text-xs" onClick={saveEntry}>
                <Plus size={12} className="inline mr-1" />Save Server
              </button>
              <button className="btn-ghost text-xs" onClick={closeForm}>Cancel</button>
            </div>
          </div>
        )}

        {mcp?.path && (
          <div className="flex flex-wrap items-center gap-2 mt-3 text-xs text-gray-600">
            <span>
              Saved to <code className="font-mono text-gray-400">{mcp.path}</code>
            </span>
            <button
              className="btn-ghost text-xs py-1 px-2"
              onClick={async () => {
                try {
                  await openPath(mcp.path);
                } catch (e) {
                  setError(String(e));
                }
              }}
            >
              <ExternalLink size={12} className="inline mr-1" />
              Open file
            </button>
            <button
              className="btn-ghost text-xs py-1 px-2"
              onClick={async () => {
                try {
                  await revealItemInDir(mcp.path);
                } catch (e) {
                  setError(String(e));
                }
              }}
            >
              <FolderOpen size={12} className="inline mr-1" />
              Show in folder
            </button>
          </div>
        )}
        {mcp?.path && (
          <p className="text-xs text-gray-500 mt-2">On Windows, .cmd shims (e.g. npx) are auto-wrapped via cmd /c.</p>
        )}
      </div>
    </div>
  );
}