import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Copy, Check, Server, Database, RefreshCw } from "lucide-react";
import type { ServerStatus, ServerInfo } from "../types";

function CopyRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {}
  };

  return (
    <div className="flex items-center gap-3 px-3 py-2.5 border border-border bg-surface-2">
      <span className="text-xs text-gray-500 w-36 shrink-0">{label}</span>
      <span className={`flex-1 text-sm text-gray-200 truncate ${mono ? "font-mono text-xs" : ""}`}>
        {value || "—"}
      </span>
      <button
        className="text-gray-600 hover:text-gray-300 transition-colors shrink-0"
        onClick={copy}
        title="Copy to clipboard"
      >
        {copied ? <Check size={14} className="text-accent-green" /> : <Copy size={14} />}
      </button>
    </div>
  );
}

export default function Api() {
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  const loadInfo = useCallback(async () => {
    setRefreshing(true);
    setError(null);
    try {
      const i = await invoke<ServerInfo>("get_server_info");
      setInfo(i);
    } catch (e) {
      setInfo(null);
      setError(String(e));
    } finally {
      setRefreshing(false);
    }
  }, []);

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

  // Refresh endpoint info when the server transitions to running
  useEffect(() => {
    if (status.type === "running") {
      loadInfo();
      const id = setInterval(loadInfo, 5000);
      return () => clearInterval(id);
    }
    setInfo(null);
    setError(null);
  }, [status.type === "running" ? "running" : "not", loadInfo]);

  if (status.type !== "running") {
    return (
      <div className="flex-1 flex flex-col items-center justify-center gap-3 p-8">
        <Server size={28} className="text-gray-600" />
        <p className="text-sm text-gray-600">Start the server to see API connection details.</p>
      </div>
    );
  }

  const apiKeyLine = info?.api_key ? `\n  "apiKey": "${info.api_key}",` : "";
  const envConfig = info
    ? `OPENAI_BASE_URL=${info.base_url}${info.api_key ? `\nOPENAI_API_KEY=${info.api_key}` : ""}\nOPENAI_MODEL=${info.model_id}`
    : "";

  return (
    <div className="flex-1 overflow-y-auto p-6 space-y-6">
      <div className="flex items-start justify-between">
        <div>
          <h1 className="text-2xl font-bold text-gray-100">API</h1>
          <p className="text-gray-500 text-sm mt-1">
            Connection details for the running server — OpenAI-compatible.
          </p>
        </div>
        <button className="btn-secondary text-xs" onClick={loadInfo} disabled={refreshing}>
          <RefreshCw size={13} className={refreshing ? "animate-spin" : ""} />
          Refresh
        </button>
      </div>

      {error && (
        <div className="card border-accent-red/30 bg-accent-red/5">
          <p className="text-sm text-accent-red">{error}</p>
        </div>
      )}

      {/* Endpoints */}
      <div className="card">
        <h2 className="section-title">Endpoints</h2>
        <div className="space-y-1.5 mt-3">
          <CopyRow label="Base URL" value={info?.base_url ?? ""} mono />
          <CopyRow label="Chat" value={`${info?.base_url ?? ""}/chat/completions`} mono />
          <CopyRow label="Completions" value={`${info?.base_url ?? ""}/completions`} mono />
          <CopyRow label="Embeddings" value={`${info?.base_url ?? ""}/embeddings`} mono />
          <CopyRow label="List models" value={`${info?.base_url ?? ""}/models`} mono />
          <CopyRow label="API key" value={info?.api_key ?? "(none — open server)"} mono />
        </div>
      </div>

      {/* Loaded model */}
      <div className="card">
        <h2 className="section-title">Loaded Model</h2>
        <div className="space-y-1.5 mt-3">
          <CopyRow label="Model ID" value={info?.model_id ?? ""} mono />
          <CopyRow label="Alias" value={info?.model_alias ?? ""} mono />
          <CopyRow label="Path" value={info?.model_path ?? ""} mono />
          <CopyRow label="Context (n_ctx)" value={info ? String(info.n_ctx) : ""} mono />
          <CopyRow label="Max tokens" value={info ? String(info.n_predict) : ""} mono />
          <CopyRow
            label="Slots"
            value={info ? `${info.slots_idle} idle / ${info.total_slots} total` : ""}
          />
        </div>
      </div>

      {/* Client config */}
      {info && (
        <div className="card">
          <h2 className="section-title">Client Configuration</h2>
          <p className="text-xs text-gray-600 mt-1 mb-3">
            Environment variables for OpenAI-compatible SDKs (Python, Node, curl…).
          </p>
          <div className="flex items-center gap-3">
            <pre className="flex-1 bg-surface-0 p-3 font-mono text-xs text-gray-300 overflow-x-auto select-text whitespace-pre-wrap">
              {envConfig}
            </pre>
            <button
              className="text-gray-600 hover:text-gray-300 transition-colors shrink-0"
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText(envConfig);
                } catch {}
              }}
              title="Copy environment variables"
            >
              <Copy size={14} />
            </button>
          </div>
          {apiKeyLine && (
            <p className="text-xs text-gray-600 mt-2">
              <Database size={11} className="inline mr-1" />
              The server was started with an API key — include it in all requests.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
