import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Channel } from "@tauri-apps/api/core";
import { useNavigate } from "react-router-dom";
import { ArrowUp, Play, RefreshCw, Square } from "lucide-react";
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

// ── Harness chat (native, streaming) ────────────────────────────────────────

interface ChatMsg {
  role: "user" | "assistant";
  content: string;
}

function HarnessChat() {
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [streamText, setStreamText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
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

  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [messages, streamText]);

  const send = async () => {
    const text = input.trim();
    if (!text || streaming) return;
    const history: ChatMsg[] = [...messages, { role: "user", content: text }];
    setMessages(history);
    setInput("");
    setStreaming(true);
    setStreamText("");
    setError(null);

    let acc = "";
    const channel = new Channel<string>();
    channel.onmessage = (raw) => {
      try {
        const ev = JSON.parse(raw);
        if (ev.type === "content" && typeof ev.text === "string") {
          acc += ev.text;
          setStreamText(acc);
        }
      } catch {}
    };

    try {
      const payload = history.map((m) => ({ role: m.role, content: m.content }));
      const finish = await invoke<string>("harness_chat_send", {
        messages: payload,
        onEvent: channel,
      });
      setMessages([...history, { role: "assistant", content: acc || `(no response — ${finish})` }]);
    } catch (e) {
      const msg = String(e);
      const aborted = msg.includes("aborted");
      setMessages([...history, { role: "assistant", content: acc + (aborted ? "  (stopped)" : "") }]);
      if (!aborted) setError(msg);
    } finally {
      setStreaming(false);
      setStreamText(null);
    }
  };

  const abort = async () => {
    try {
      await invoke("harness_chat_abort");
    } catch {}
  };

  if (status.type === "starting") {
    return <ServerStarting />;
  }
  if (status.type !== "running") {
    return <ServerStopped />;
  }
  const port = status.type === "running" ? status.port : 0;

  const empty = messages.length === 0 && streamText === null;

  return (
    <div className="flex-1 flex flex-col min-h-0">
      {/* Messages */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto px-6 py-4 space-y-4">
        {empty && (
          <div className="h-full flex flex-col items-center justify-center gap-2 text-center">
            <p className="text-base font-semibold text-gray-200">Catapult Chat</p>
            <p className="text-sm text-gray-500">
              Streaming from llama-server on port {port}. Agent tools arrive with the harness.
            </p>
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`flex ${m.role === "user" ? "justify-end" : "justify-start"}`}>
            <div
              className={`max-w-[80%] rounded px-3 py-2 text-sm whitespace-pre-wrap break-words ${
                m.role === "user"
                  ? "bg-primary/20 text-gray-100"
                  : "bg-surface-2 text-gray-200"
              }`}
            >
              {m.content}
            </div>
          </div>
        ))}
        {streamText !== null && (
          <div className="flex justify-start">
            <div className="max-w-[80%] rounded px-3 py-2 text-sm bg-surface-2 text-gray-200 whitespace-pre-wrap break-words">
              {streamText}
              {streaming && <span className="ml-0.5 inline-block w-2 h-4 bg-gray-500 animate-pulse align-middle" />}
            </div>
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
          <button className="btn-danger shrink-0" onClick={abort} title="Stop generation">
            <Square size={13} />
            Stop
          </button>
        ) : (
          <button
            className="btn-primary shrink-0"
            onClick={send}
            disabled={!input.trim()}
            title="Send"
          >
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
