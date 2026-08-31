import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useNavigate } from "react-router-dom";
import { Play, RefreshCw } from "lucide-react";
import type { ServerStatus } from "../types";

export default function Chat() {
  const navigate = useNavigate();
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const iframeRef = useRef<HTMLIFrameElement>(null);

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

  useEffect(() => {
    if (iframeRef.current && chatUrl && iframeRef.current.src !== chatUrl) {
      iframeRef.current.src = chatUrl;
    }
  }, [chatUrl]);

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

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="flex items-center justify-between px-4 py-2 border-b border-border">
        <span className="text-xs text-gray-500 font-mono">{chatUrl}</span>
      </div>
      <iframe ref={iframeRef} src={chatUrl} className="flex-1 w-full border-0" allow="clipboard-write" title="llama.cpp Chat" />
    </div>
  );
}
