import { useEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useNavigate } from "react-router-dom";
import { ArrowUp, Check, FileWarning, Hammer, Play, RefreshCw, Square, Wrench, X } from "lucide-react";
import type { ServerStatus } from "../types";

// ── Shared server-gate states ───────────────────────────────────────────────

function ServerStarting() {
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

function ServerStopped() {
  const navigate = useNavigate();
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

// ── Harness chat (agent loop with sandboxed tools) ──────────────────────────

type Item =
  | { kind: "msg"; role: "user" | "assistant"; content: string }
  | { kind: "tool"; callId: string; tool: string; args: string; output?: { ok: boolean; text: string } }
  | {
      kind: "approval";
      seq: number;
      tool: string;
      command: string | null;
      args: string;
      resolved?: "denied" | "once" | "session";
    };

function HarnessChat() {
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const [items, setItems] = useState<Item[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [streamText, setStreamText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const approvalSeq = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const poll = async () => {
      try {
        setStatus(await invoke<ServerStatus>("get_server_status"));
      } catch {}
    };
    poll();
    const id = setInterval(poll, 2000);
    return () => clearInterval(id);
  }, []);

  // Approval prompts arrive via a global event while the send invoke is still
  // pending (the loop parks until the user decides).
  useEffect(() => {
    const unlisten = listen<{
      type: string;
      tool: string;
      command: string | null;
      args: string;
    }>("harness_approval", (event) => {
      const p = event.payload;
      approvalSeq.current += 1;
      const seq = approvalSeq.current;
      setItems((prev) => [
        ...prev,
        { kind: "approval", seq, tool: p.tool, command: p.command, args: p.args },
      ]);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [items, streamText]);

  const send = async () => {
    const text = input.trim();
    if (!text || streaming) return;
    setItems((prev) => [...prev, { kind: "msg", role: "user", content: text }]);
    setInput("");
    setStreaming(true);
    setStreamText("");
    setError(null);

    let acc = "";
    const channel = new Channel<string>();
    channel.onmessage = (raw) => {
      let ev: Record<string, unknown>;
      try {
        ev = JSON.parse(raw);
      } catch {
        return;
      }
      switch (ev.type) {
        case "content":
          acc += ev.text ?? "";
          setStreamText(acc);
          break;
        case "tool_call":
          setStreamText(null);
          setItems((prev) => [
            ...prev,
            {
              kind: "tool",
              callId: ev.call_id ?? "",
              tool: ev.tool ?? "?",
              args: ev.args ?? "",
            } as Item,
          ]);
          break;
        case "tool_result":
          setItems((prev) =>
            prev.map((it): Item =>
              it.kind === "tool" && it.callId === ev.call_id && it.output === undefined
                ? { ...it, output: { ok: !!ev.ok, text: String(ev.output ?? "") } }
                : it,
            ),
          );
          break;
        case "subagent_spawned":
          setStreamText(null);
          setItems((prev) => [
            ...prev,
            {
              kind: "tool",
              callId: ev.call_id ?? "",
              tool: `⟳ ${ev.kind} subagent`,
              args: ev.goal ?? "",
            } as Item,
          ]);
          break;
        case "subagent_finished":
          setItems((prev) =>
            prev.map((it): Item =>
              it.kind === "tool" && it.callId === ev.call_id && it.output === undefined
                ? { ...it, output: { ok: !!ev.ok, text: String(ev.summary ?? "") } }
                : it,
            ),
          );
          break;
        default:
          break;
      }
    };

    try {
      const finish = await invoke<string>("harness_agent_send", { message: text, onEvent: channel });
      setItems((prev) => [
        ...prev,
        { kind: "msg", role: "assistant", content: acc || `(no response — ${finish})` } as Item,
      ]);
    } catch (e) {
      const msg = String(e);
      const aborted = msg.includes("aborted");
      if (acc) {
        setItems((prev) => [
          ...prev,
          { kind: "msg", role: "assistant", content: acc + (aborted ? "  (stopped)" : "") },
        ]);
      }
      if (!aborted) setError(msg);
    } finally {
      setStreaming(false);
      setStreamText(null);
    }
  };

  const decide = async (seq: number, grant: "once" | "session" | null) => {
    setItems((prev) =>
      prev.map((it) =>
        it.kind === "approval" && it.seq === seq && !it.resolved
          ? { ...it, resolved: grant ?? "denied" }
          : it,
      ),
    );
    try {
      await invoke("harness_agent_decide", { grant });
    } catch {}
  };

  const resetChat = async () => {
    if (streaming) return;
    try {
      await invoke("harness_agent_reset");
    } catch {}
    setItems([]);
    setError(null);
  };

  if (status.type === "starting") {
    return <ServerStarting />;
  }
  if (status.type !== "running") {
    return <ServerStopped />;
  }

  return (
    <div className="flex-1 flex flex-col min-h-0">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-1.5 border-b border-border">
        <span className="text-xs text-gray-500 flex items-center gap-1.5">
          <Hammer size={12} className="text-primary-light" />
          Agent · sandboxed tools
        </span>
        <button
          className="text-xs text-gray-500 hover:text-gray-300"
          onClick={resetChat}
          disabled={streaming}
          title="Start a new conversation"
        >
          New chat
        </button>
      </div>

      {/* Messages */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto px-6 py-4 space-y-3">
        {items.length === 0 && streamText === null && (
          <div className="h-full flex flex-col items-center justify-center gap-2 text-center">
            <p className="text-base font-semibold text-gray-200">Catapult Chat</p>
            <p className="text-sm text-gray-500 max-w-md">
              The agent can read, search, create and edit files inside the sandboxed project
              directory, and run commands with your approval.
            </p>
          </div>
        )}
        {items.map((it, i) => {
          if (it.kind === "msg") {
            return (
              <div key={i} className={`flex ${it.role === "user" ? "justify-end" : "justify-start"}`}>
                <div
                  className={`max-w-[80%] rounded px-3 py-2 text-sm whitespace-pre-wrap break-words ${
                    it.role === "user" ? "bg-primary/20 text-gray-100" : "bg-surface-2 text-gray-200"
                  }`}
                >
                  {it.content}
                </div>
              </div>
            );
          }
          if (it.kind === "tool") {
            return (
              <div key={i} className="flex justify-start">
                <div className="max-w-[85%] rounded border border-border bg-surface-2 px-3 py-2 text-xs">
                  <p className="text-gray-400 flex items-center gap-1.5 mb-1">
                    <Wrench size={11} className="text-gray-500 shrink-0" />
                    <span className="text-gray-300 font-medium">{it.tool}</span>
                    <span className="text-gray-600 truncate">{it.args}</span>
                  </p>
                  {it.output && (
                    <pre
                      className={`whitespace-pre-wrap break-words text-[11px] leading-snug max-h-40 overflow-y-auto ${
                        it.output.ok ? "text-gray-400" : "text-accent-yellow"
                      }`}
                    >
                      {it.output.text}
                    </pre>
                  )}
                </div>
              </div>
            );
          }
          // Approval card
          return (
            <div key={i} className="flex justify-start">
              <div className="max-w-[85%] rounded border border-accent-yellow/40 bg-accent-yellow/5 px-3 py-2 text-xs">
                <p className="text-gray-300 flex items-center gap-1.5 mb-1">
                  <FileWarning size={11} className="text-accent-yellow shrink-0" />
                  Approval requested: <span className="font-medium">{it.tool}</span>
                  {it.command && <span className="badge-gray text-[10px]">{it.command}</span>}
                </p>
                <pre className="whitespace-pre-wrap break-words text-[11px] text-gray-400 mb-2 max-h-40 overflow-y-auto">
                  {it.args}
                </pre>
                {!it.resolved ? (
                  <div className="flex items-center gap-2">
                    <button className="btn-secondary py-1 px-2" onClick={() => decide(it.seq, "once")}>
                      <Check size={11} /> Allow once
                    </button>
                    <button className="btn-secondary py-1 px-2" onClick={() => decide(it.seq, "session")}>
                      <Check size={11} /> Allow session (30 min)
                    </button>
                    <button className="btn-ghost py-1 px-2 text-accent-red" onClick={() => decide(it.seq, null)}>
                      <X size={11} /> Deny
                    </button>
                  </div>
                ) : (
                  <p className="text-gray-600">
                    {it.resolved === "denied" ? "Denied" : `Allowed (${it.resolved})`}
                  </p>
                )}
              </div>
            </div>
          );
        })}
        {streamText !== null && (
          <div className="flex justify-start">
            <div className="max-w-[80%] rounded px-3 py-2 text-sm bg-surface-2 text-gray-200 whitespace-pre-wrap break-words">
              {streamText}
              {streaming && <span className="ml-0.5 inline-block w-2 h-4 bg-gray-500 animate-pulse align-middle" />}
            </div>
          </div>
        )}
        {streaming && streamText === null && items.length > 0 && (
          <div className="flex justify-start">
            <RefreshCw size={13} className="animate-spin text-gray-500" />
          </div>
        )}
      </div>

      {error && (
        <div className="px-6 pb-2">
          <p className="text-xs text-accent-red break-words">{error}</p>
        </div>
      )}

      {/* Input */}
      <div className="border-t border-border p-3 flex items-end gap-2">
        <textarea
          className="input flex-1 resize-none h-16 text-sm"
          placeholder="Send a message…"
          value={input}
          disabled={streaming}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        {streaming ? (
          <button className="btn-danger shrink-0" onClick={() => invoke("harness_agent_abort").catch(() => {})} title="Stop">
            <Square size={13} />
            Stop
          </button>
        ) : (
          <button className="btn-primary shrink-0" onClick={send} disabled={!input.trim()} title="Send">
            <ArrowUp size={14} />
          </button>
        )}
      </div>
    </div>
  );
}

// ── Classic WebUI (fallback, fully supported) ───────────────────────────────

function WebUIChat() {
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const iframeRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    const poll = async () => {
      try {
        setStatus(await invoke<ServerStatus>("get_server_status"));
      } catch {}
    };
    poll();
    const id = setInterval(poll, 2000);
    return () => clearInterval(id);
  }, []);

  const port = status.type === "running" ? status.port : null;
  const chatUrl = port ? `http://127.0.0.1:${port}` : "";

  useEffect(() => {
    if (iframeRef.current && chatUrl && iframeRef.current.src !== chatUrl) {
      iframeRef.current.src = chatUrl;
    }
  }, [chatUrl]);

  if (status.type === "starting") {
    return <ServerStarting />;
  }
  if (status.type !== "running" || !port) {
    return <ServerStopped />;
  }

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="flex items-center justify-between px-4 py-2 border-b border-border">
        <span className="text-xs text-gray-500 font-mono">{chatUrl}</span>
      </div>
      <iframe ref={iframeRef} src={chatUrl} className="flex-1 w-full border-0" allow="clipboard-write" title="llama.cpp Chat" />
    </div>
  );
}

// ── Root ────────────────────────────────────────────────────────────────────

export default function Chat() {
  const [harnessChat, setHarnessChat] = useState<boolean | null>(null);

  useEffect(() => {
    invoke<{ harness_chat: boolean }>("get_config")
      .then((c) => setHarnessChat(c.harness_chat ?? true))
      .catch(() => setHarnessChat(true));
  }, []);

  if (harnessChat === false) {
    return <WebUIChat />;
  }
  return <HarnessChat />;
}
