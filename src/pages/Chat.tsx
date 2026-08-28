import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useNavigate } from "react-router-dom";
import { Play, RefreshCw, Wrench, Globe, AlertTriangle, CheckCircle2, Info } from "lucide-react";
import type { ServerStatus, ServerToolInfo } from "../types";

let persistentIframe: HTMLIFrameElement | null = null;

export default function Chat() {
  const navigate = useNavigate();
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const [tools, setTools] = useState<ServerToolInfo[] | null>(null);
  const [toolsError, setToolsError] = useState<string | null>(null);
  const [toolsLoading, setToolsLoading] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const poll = async () => {
      try {
        const s = await invoke<ServerStatus>("get_server_status");
        setStatus(s);
      } catch {}
    };
    poll();
    const id = setInterval(poll, 2000);
    return () => clearInterval(id);
  }, []);

  const port = status.type === "running" ? status.port : null;
  const chatUrl = port ? `http://127.0.0.1:${port}` : "";

  const fetchTools = useCallback(async () => {
    if (status.type !== "running" || !port) {
      setTools(null);
      setToolsError(null);
      return;
    }
    setToolsLoading(true);
    setToolsError(null);
    try {
      const t = await invoke<ServerToolInfo[]>("get_server_tools");
      setTools(t);
    } catch (e) {
      setTools(null);
      setToolsError(String(e));
    } finally {
      setToolsLoading(false);
    }
  }, [status.type, port]);

  useEffect(() => {
    if (status.type === "running") {
      fetchTools();
      const id = setInterval(fetchTools, 5000);
      return () => clearInterval(id);
    } else {
      setTools(null);
      setToolsError(null);
    }
  }, [status.type, fetchTools]);

  // Create persistent iframe once, reattach on mount
  useEffect(() => {
    if (status.type !== "running" || !port || !containerRef.current) return;

    if (!persistentIframe) {
      persistentIframe = document.createElement("iframe");
      persistentIframe.className = "flex-1 w-full border-0";
      persistentIframe.allow = "clipboard-write";
      persistentIframe.title = "llama.cpp Chat";
    }

    if (persistentIframe.src !== chatUrl) {
      persistentIframe.src = chatUrl;
    }

    if (!persistentIframe.parentNode) {
      containerRef.current.appendChild(persistentIframe);
    }

    return () => {
      if (persistentIframe?.parentNode) {
        persistentIframe.parentNode.removeChild(persistentIframe);
      }
    };
  }, [status.type === "running" ? "running" : "not", port]);

  if (status.type === "starting") {
    return (
      <div className="flex-1 flex flex-col items-center justify-center gap-5 p-8">
        <div className="text-center">
          <p className="text-base font-semibold text-gray-200">Server is starting…</p>
          <p className="text-sm text-gray-500 mt-1">
            The model is loading. This may take a moment.
          </p>
        </div>
        <RefreshCw size={20} className="animate-spin text-gray-500" />
      </div>
    );
  }

  if (status.type !== "running" || !port) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center gap-5 p-8">
        <div className="text-center">
          <p className="text-base font-semibold text-gray-200">Server is not running</p>
          <p className="text-sm text-gray-500 mt-1">
            Start the server first to use the chat.
          </p>
        </div>
        <button className="btn-primary" onClick={() => navigate("/server")}>
          <Play size={15} />
          Go to Run
        </button>
      </div>
    );
  }

  const serverTools = tools?.filter((t) => t.type === "server") ?? [];
  const mcpTools = tools?.filter((t) => t.type === "mcp") ?? [];
  const hasMcp = mcpTools.length > 0;

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="flex items-center justify-between px-4 py-2 border-b border-border">
        <span className="text-xs text-gray-500 font-mono">{chatUrl}</span>
        <button className="btn-ghost text-xs py-1 px-2" onClick={fetchTools} title="Refresh tool list">
          <RefreshCw size={12} className={toolsLoading ? "animate-spin" : ""} /> Refresh tools
        </button>
      </div>

      {/* MCP / Tools status banner */}
      <div className="px-4 py-3 border-b border-border bg-surface-1">
        {toolsLoading && !tools ? (
          <p className="text-xs text-gray-500 flex items-center gap-2">
            <RefreshCw size={12} className="animate-spin" /> Checking server tools…
          </p>
        ) : toolsError ? (
          <div className="flex items-start gap-2 text-xs">
            <AlertTriangle size={14} className="text-accent-yellow mt-0.5 shrink-0" />
            <div>
              <p className="text-gray-300">Could not fetch server tools: <span className="font-mono text-accent-yellow">{toolsError}</span></p>
              <p className="text-gray-500 mt-1">The server may still be starting. The embedded chat below is the llama.cpp WebUI – its tool list is fetched from <code className="font-mono">/tools</code>.</p>
            </div>
          </div>
        ) : tools ? (
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span className="flex items-center gap-1.5">
                <Wrench size={12} className="text-gray-400" />
                <span className="text-gray-400">Server tools:</span>
                <span className="text-gray-200">{serverTools.length}</span>
              </span>
              <span className="text-gray-600">•</span>
              <span className="flex items-center gap-1.5">
                <Globe size={12} className={hasMcp ? "text-accent-green" : "text-gray-500"} />
                <span className={hasMcp ? "text-accent-green" : "text-gray-500"}>MCP tools:</span>
                <span className={hasMcp ? "text-accent-green font-medium" : "text-gray-500"}>{mcpTools.length}</span>
                {hasMcp ? <CheckCircle2 size={12} className="text-accent-green" /> : <AlertTriangle size={12} className="text-accent-yellow" />}
              </span>
              {hasMcp && (
                <span className="text-gray-500">
                  ({mcpTools.map((t) => t.name).join(", ")})
                </span>
              )}
            </div>
            {hasMcp ? (
              <div className="flex items-start gap-2 text-xs bg-accent-green/5 border border-accent-green/20 px-3 py-2">
                <Info size={12} className="text-accent-green mt-0.5 shrink-0" />
                <div className="text-gray-400 leading-relaxed">
                  <span className="text-accent-green font-medium">MCP is active.</span> In the chat below, open the WebUI’s <em>Tools</em> (or <em>Settings → Tools</em>) and ensure the <code className="font-mono text-gray-300">MCP</code> category and the <code className="font-mono text-gray-300">{mcpTools[0]?.name}</code> entry are enabled (per-conversation toggles). Then ask e.g. <code className="font-mono text-gray-200">“Use exa_web_search_exa to find the top 3 worldwide news”</code>. If the model says it can’t browse, explicitly name the tool.
                  <br />
                  <span className="text-gray-500">Tip: Bonsai Q1 is a 1-bit quant – tool-calling is more reliable with a higher quant (Q4+) or by adding a system prompt like “You have web search tools, use them when asked for current info.” The server was started with <code className="font-mono">--mcp-servers-config</code> pointing at the wrapped <code className="font-mono">mcp.json</code> (<code className="font-mono">cmd /c npx …</code> on Windows); check <em>Run → Server Logs</em> for <code className="font-mono">MCP warmup: ‘exa’ discovered 2 tools</code>.</span>
                </div>
              </div>
            ) : (
              <div className="flex items-start gap-2 text-xs bg-accent-yellow/5 border border-accent-yellow/20 px-3 py-2">
                <AlertTriangle size={12} className="text-accent-yellow mt-0.5 shrink-0" />
                <div className="text-gray-400">
                  No MCP tools were advertised by the server. Check <button className="underline text-primary-light" onClick={() => navigate("/tools")}>Tools → MCP Servers</button> and ensure an <code className="font-mono">exa</code> server with command <code className="font-mono">npx</code> (auto-wrapped to <code className="font-mono">cmd /c npx</code> on Windows) is saved, then restart the server from <em>Run</em>. The log should show <code className="font-mono">Added 2 MCP tools</code>.
                </div>
              </div>
            )}
          </div>
        ) : null}
      </div>

      <div ref={containerRef} className="flex-1 flex flex-col min-h-0" />
    </div>
  );
}
