import { useEffect, useRef, useState, Fragment } from "react";
import type { ReactNode } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useNavigate } from "react-router-dom";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import remarkBreaks from "remark-breaks";
import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";
import {
  ArrowUp,
  Brain,
  Check,
  Copy,
  Eye,
  FileWarning,
  FolderOpen,
  Paperclip,
  Play,
  Plus,
  RefreshCw,
  Square,
  ChevronDown,
  Trash2,
  Wrench,
  X,
} from "lucide-react";
import type {
  ServerStatus,
  HarnessRunResult,
  SessionInfo,
  HarnessCapabilities,
  ChatAttachment,
} from "../types";

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

// ── Items (messages + tool activity) ────────────────────────────────────────

type Item =
  | {
      kind: "msg";
      role: "user" | "assistant";
      content: string;
      model?: string;
      tokps?: number | null;
      time?: number;
      elapsedMs?: number;
      tokens?: number;
      reasoning?: string;
      attachments?: string[];
    }
  | { kind: "tool"; callId: string; tool: string; args: string; output?: { ok: boolean; text: string } }
  | {
      kind: "approval";
      seq: number;
      tool: string;
      command: string | null;
      args: string;
      resolved?: "denied" | "once" | "session";
    };

const REASONING_OPTIONS = [
  { value: "", label: "Default" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "max", label: "Max" },
  { value: "xhigh", label: "X-High" },
] as const;

const REMARK_PLUGINS = [remarkGfm, remarkMath, remarkBreaks];
const REHYPE_PLUGINS = [rehypeKatex];

function Markdown({ content }: { content: string }) {
  return (
    <div className="md select-text">
      <ReactMarkdown remarkPlugins={REMARK_PLUGINS} rehypePlugins={REHYPE_PLUGINS}>
        {content}
      </ReactMarkdown>
    </div>
  );
}

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// ── Sidebar: projects + sessions ────────────────────────────────────────────

// ── Sidebar: projects + sessions ────────────────────────────────────────────

function ChatSidebar({ onProjectChanged, onSessionPicked }: {
  onProjectChanged: () => void;
  onSessionPicked: () => void;
}) {
  const [projects, setProjects] = useState<{ id: string; name: string; path: string }[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);

  const refreshProjects = async () => {
    try {
      const c = await invoke<{ harness_projects: typeof projects; harness_active_project: string | null }>("get_config");
      setProjects(c.harness_projects ?? []);
      setActive(c.harness_active_project);
    } catch {}
  };

  const refreshSessions = async () => {
    try {
      setSessions(await invoke<SessionInfo[]>("harness_sessions_list"));
    } catch {}
  };

  useEffect(() => {
    refreshProjects();
    refreshSessions();
    const id = setInterval(refreshSessions, 5000);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const addProject = async () => {
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked === "string" && picked) {
      await invoke("harness_project_add", { path: picked }).catch(() => {});
      await refreshProjects();
      onProjectChanged();
    }
  };

  const removeProject = async (id: string) => {
    await invoke("harness_project_remove", { id }).catch(() => {});
    await refreshProjects();
    onProjectChanged();
  };

  const activateProject = async (id: string | null) => {
    await invoke("harness_project_active", { id }).catch(() => {});
    setActive(id);
    onProjectChanged();
  };

  const deleteSession = async (id: string) => {
    await invoke("harness_session_delete", { id }).catch(() => {});
    await refreshSessions();
    onSessionPicked();
  };

  const loadSession = async (id: string) => {
    await invoke("harness_session_load", { id }).catch(() => {});
    onSessionPicked();
  };

  return (
    <aside className="w-60 shrink-0 border-r border-border bg-surface-1 flex flex-col overflow-y-auto">
      {/* Projects */}
      <div className="p-3 border-b border-border">
        <div className="flex items-center justify-between px-1 mb-1.5">
          <span className="text-[11px] font-semibold uppercase tracking-wide text-gray-500">Projects</span>
          <button
            className="text-gray-500 hover:text-gray-200 transition-colors"
            onClick={addProject}
            title="Add a project (working directory)"
          >
            <Plus size={13} />
          </button>
        </div>
        <div className="space-y-0.5">
          {projects.map((p) => (
            <div
              key={p.id}
              className={`group flex items-center gap-2 px-2 py-1.5 rounded text-xs cursor-pointer transition-colors ${
                p.id === active ? "bg-primary/20 text-primary-light" : "text-gray-400 hover:text-gray-200 hover:bg-primary/10"
              }`}
              onClick={() => activateProject(p.id)}
              title={p.path}
            >
              <FolderOpen size={12} className="shrink-0" />
              <span className="flex-1 truncate">{p.name}</span>
              <button
                className="opacity-0 group-hover:opacity-100 text-gray-600 hover:text-accent-red"
                onClick={(e) => { e.stopPropagation(); removeProject(p.id); }}
                title="Remove project (files stay untouched)"
              >
                <Trash2 size={11} />
              </button>
            </div>
          ))}
          {projects.length === 0 && (
            <p className="text-[11px] text-gray-600 px-2 leading-snug">
              Add a folder to sandbox the agent to a project.
            </p>
          )}
        </div>
      </div>

      {/* Sessions */}
      <div className="flex-1 overflow-y-auto p-3">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-gray-500 px-1">Sessions</span>
        <div className="space-y-0.5 mt-1.5">
          {sessions.map((s) => (
            <div
              key={s.id}
              className="group flex items-center gap-2 px-2 py-1.5 rounded text-xs text-gray-400 hover:text-gray-200 hover:bg-primary/10 cursor-pointer transition-colors"
              onClick={() => loadSession(s.id)}
              title={new Date(s.updated * 1000).toLocaleString()}
            >
              <span className="flex-1 truncate">{s.title}</span>
              <button
                className="opacity-0 group-hover:opacity-100 text-gray-600 hover:text-accent-red"
                onClick={(e) => { e.stopPropagation(); deleteSession(s.id); }}
                title="Delete session"
              >
                <Trash2 size={11} />
              </button>
            </div>
          ))}
          {sessions.length === 0 && (
            <p className="text-[11px] text-gray-600 px-2 leading-snug">No sessions yet.</p>
          )}
        </div>
      </div>
    </aside>
  );
}

// ── Response footer (model, tok/s, time, tokens, copy, delete) ──────────────

function ResponseFooter({ model, tokps, elapsedMs, tokens, onCopy, onDelete }: {
  model?: string;
  tokps?: number | null;
  elapsedMs?: number;
  tokens?: number;
  onCopy: () => void;
  onDelete?: () => void;
}) {
  const [copied, setCopied] = useState(false);
  // Metadata segments joined with a small dot separator.
  const parts: { key: string; node: ReactNode }[] = [];
  if (model) parts.push({ key: "model", node: <span className="font-mono truncate max-w-[200px]">{model}</span> });
  if (tokps != null && tokps > 0) parts.push({ key: "tokps", node: <span className="tabular-nums">{tokps.toFixed(1)} t/s</span> });
  if (tokens != null && tokens > 0) parts.push({ key: "tokens", node: <span className="tabular-nums">{tokens} tok</span> });
  if (elapsedMs != null && elapsedMs > 0) {
    parts.push({
      key: "elapsed",
      node: <span className="tabular-nums">{elapsedMs >= 1000 ? `${(elapsedMs / 1000).toFixed(1)}s` : `${elapsedMs}ms`}</span>,
    });
  }
  return (
    <div className="flex items-center gap-1.5 mt-1 px-1 text-[10px] text-gray-600">
      {parts.map((p, i) => (
        <Fragment key={p.key}>
          {i > 0 && <span className="text-gray-700 select-none">·</span>}
          {p.node}
        </Fragment>
      ))}
      <button
        className="ml-auto inline-flex items-center gap-1 hover:text-gray-300 transition-colors"
        onClick={() => {
          onCopy();
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
        title="Copy response"
      >
        {copied ? <Check size={10} /> : <Copy size={10} />}
        {copied ? "Copied" : "Copy"}
      </button>
      {onDelete && (
        <button className="inline-flex items-center gap-1 hover:text-accent-red transition-colors" onClick={onDelete} title="Delete this response and rewind to your message">
          <Trash2 size={10} />
          Delete
        </button>
      )}
    </div>
  );
}

// ── Collapsible reasoning block ("Thinking…") ───────────────────────────────

function ReasoningBlock({ text, streaming, open, onToggle }: {
  text: string;
  streaming?: boolean;
  open?: boolean;
  onToggle?: () => void;
}) {
  const [openLocal, setOpenLocal] = useState(false);
  const isOpen = open ?? openLocal;
  const toggle = onToggle ?? (() => setOpenLocal((v) => !v));
  return (
    <div className="rounded border border-border bg-surface-3/60 px-2.5 py-1.5 text-[11px] text-gray-400 mb-1.5">
      <button
        className="w-full flex items-center gap-1.5 text-left"
        onClick={toggle}
        title={isOpen ? "Hide reasoning" : "Show reasoning"}
      >
        <Brain size={11} className="text-gray-500 shrink-0" />
        <span className="text-gray-500">
          {streaming ? "Thinking…" : isOpen ? "Reasoning" : "Show reasoning"}
        </span>
        <span className={`ml-auto transition-transform ${isOpen ? "rotate-180" : ""}`}>
          <ChevronDown size={12} />
        </span>
      </button>
      {isOpen && (
        <pre className="whitespace-pre-wrap break-words text-[11px] leading-snug mt-1.5 max-h-56 overflow-y-auto select-text">
          {text}
        </pre>
      )}
    </div>
  );
}

// ── Context ring (usage vs. model context + session avg tok/s) ──────────────

function ContextRing({ used, total, avgTokps }: {
  used: number | null;
  total: number | null;
  avgTokps: number | null;
}) {
  const pct =
    used != null && total != null && total > 0 ? Math.min(1, used / total) : 0;
  const r = 13;
  const c = 2 * Math.PI * r;
  const color =
    pct >= 0.9 ? "stroke-accent-red" : pct >= 0.7 ? "stroke-accent-yellow" : "stroke-primary";
  const usedStr = used != null ? used.toLocaleString() : "–";
  const totalStr = total != null ? total.toLocaleString() : "–";
  const avgStr =
    avgTokps != null && avgTokps > 0 ? ` · avg ${avgTokps.toFixed(1)} t/s` : "";
  return (
    <div
      className="relative shrink-0 w-9 h-9"
      title={`Context: ${usedStr} / ${totalStr} tokens${avgStr}`}
    >
      <svg viewBox="0 0 32 32" className="w-9 h-9 -rotate-90">
        <circle cx="16" cy="16" r={r} fill="none" className="stroke-surface-4" strokeWidth="3" />
        <circle
          cx="16"
          cy="16"
          r={r}
          fill="none"
          className={color}
          strokeWidth="3"
          strokeLinecap="round"
          strokeDasharray={`${(c * pct).toFixed(1)} ${c.toFixed(1)}`}
        />
      </svg>
      <span className="absolute inset-0 flex items-center justify-center text-[8px] tabular-nums text-gray-400">
        {Math.round(pct * 100)}%
      </span>
    </div>
  );
}

// ── Harness chat (agent loop with sandboxed tools) ──────────────────────────

function HarnessChat() {
  const navigate = useNavigate();
  const [status, setStatus] = useState<ServerStatus>({ type: "stopped" });
  const [items, setItems] = useState<Item[]>([]);
  const [tools, setTools] = useState<ToolListing[] | null>(null);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [streamText, setStreamText] = useState<string | null>(null);
  // Reasoning buffer for the in-flight run (shown as a collapsible block).
  const [reasoningText, setReasoningText] = useState<string | null>(null);
  const [reasoningOpen, setReasoningOpen] = useState(false);
  const [reasoningEffort, setReasoningEffort] = useState("");
  const [caps, setCaps] = useState<HarnessCapabilities | null>(null);
  const [attachments, setAttachments] = useState<ChatAttachment[]>([]);
  const [contextUsed, setContextUsed] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [activeProject, setActiveProject] = useState<string | null>(null);
  // Run status shown under the input: the model is warming up/loading vs. the
  // agent is actively reasoning over the request.
  const [runStatus, setRunStatus] = useState<"thinking" | "loading" | null>(null);
  const approvalSeq = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);

  const refreshActiveProject = async () => {
    try {
      const c = await invoke<{ harness_active_project: string | null }>("get_config");
      setActiveProject(c.harness_active_project);
    } catch {}
  };

  useEffect(() => {
    const poll = async () => {
      try {
        setStatus(await invoke<ServerStatus>("get_server_status"));
      } catch {}
    };
    invoke<ToolListing[]>("harness_agent_tools").then(setTools).catch(() => {});
    invoke<HarnessCapabilities>("harness_agent_capabilities").then(setCaps).catch(() => {});
    refreshActiveProject();
    // The transcript lives in the backend — restore it so chats are
    // consultable even when the server is stopped.
    restoreFromBackend();
    poll();
    const id = setInterval(poll, 2000);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Rebuild the visible transcript from the backend (after a session load,
  // project switch, or reset). User + plain assistant turns are restored;
  // tool-call detail lives server-side.
  const restoreFromBackend = async () => {
    try {
      const messages = await invoke<{ role: string; content?: string | null; tool_calls?: unknown }[]>(
        "harness_agent_history",
      );
      const restored: Item[] = [];
      for (const m of messages) {
        if (m.role === "user" && m.content) {
          restored.push({ kind: "msg", role: "user", content: m.content });
        } else if (m.role === "assistant" && m.content && !m.tool_calls) {
          restored.push({ kind: "msg", role: "assistant", content: m.content });
        }
      }
      setItems(restored);
      setError(null);
    } catch {}
  };

  // Average generation speed across the current session's assistant turns.
  const avgTokps = (() => {
    const vals = items.flatMap((it) =>
      it.kind === "msg" && it.role === "assistant" && it.tokps != null && it.tokps > 0 ? [it.tokps as number] : [],
    );
    return vals.length > 0 ? vals.reduce((a, b) => a + b, 0) / vals.length : null;
  })();

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
  }, [items, streamText, reasoningText]);

  const send = async () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || streaming) return;
    setItems((prev) => [
      ...prev,
      {
        kind: "msg",
        role: "user",
        content: text || "(attachments only)",
        time: Date.now(),
        attachments: attachments.map((a) => a.name),
      } as Item,
    ]);
    setInput("");
    setStreaming(true);
    setStreamText("");
    setReasoningText(null);
    setReasoningOpen(false);
    setRunStatus("thinking");
    setError(null);

    let acc = "";
    let reasoningAcc = "";
    let contentStarted = false;
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
          // The answer started — collapse the reasoning block automatically
          // (the user can still expand it manually afterwards).
          if (!contentStarted) {
            contentStarted = true;
            setReasoningOpen(false);
          }
          break;
        case "reasoning":
          reasoningAcc += ev.text ?? "";
          setReasoningText(reasoningAcc);
          // Collapsed by default ("Thinking…" label) — expandable via chevron.
          break;
        case "tool_call":
          setStreamText(null);
          setReasoningOpen(false);
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
        case "notice":
          if (typeof ev.text === "string" && ev.text) {
            // Model loading/switching: promote the run status line so the
            // user sees "Loading…" instead of just a transcript card.
            if (/loading/i.test(ev.text)) {
              setRunStatus("loading");
            }
            setItems((prev) => [
              ...prev,
              { kind: "tool", callId: `notice-${Date.now()}`, tool: "note", args: ev.text as string },
            ]);
          }
          break;
        default:
          break;
      }
    };

    try {
      const res = await invoke<HarnessRunResult>("harness_agent_send", {
        message: text,
        reasoningEffort: reasoningEffort || null,
        attachments: attachments.map((a) => ({
          name: a.name,
          kind: a.kind,
          data_base64:
            a.kind === "image" && a.preview ? a.preview.split(",", 2)[1] ?? null : null,
          text: a.kind === "text" ? (a.text ?? "") : null,
        })),
        onEvent: channel,
      });
      setContextUsed(res.prompt_tokens ?? null);
      setItems((prev) => [
        ...prev,
        {
          kind: "msg",
          role: "assistant",
          content: res.text || acc || "(no response)",
          model: res.model,
          tokps: res.tokens_per_sec ?? null,
          elapsedMs: res.elapsed_ms,
          tokens: res.gen_tokens,
          reasoning: reasoningAcc || undefined,
        } as Item,
      ]);
      setAttachments([]);
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
      setAttachments([]);
      setRunStatus(null);
    }
  };

  // ── Attachments (+ button): images for vision models, text inline ──────────

  const attachFiles = async () => {
    let picked: string | string[] | null = null;
    try {
      picked = await openDialog({ multiple: true, directory: false });
    } catch {
      return;
    }
    const paths: string[] = Array.isArray(picked) ? picked : picked ? [picked] : [];
    for (const path of paths) {
      try {
        const read = await invoke<{
          kind: string;
          name: string;
          data_base64?: string;
          text?: string;
        }>("harness_read_attachment", { path });
        if (read.kind === "image" && read.data_base64) {
          const ext = (read.name.split(".").pop() ?? "png").toLowerCase();
          const mime =
            ext === "jpg" || ext === "jpeg"
              ? "image/jpeg"
              : ext === "webp"
                ? "image/webp"
                : "image/png";
          setAttachments((prev) => [
            ...prev,
            { name: read.name, kind: "image", path, preview: `data:${mime};base64,${read.data_base64}` },
          ]);
        } else if (read.text != null) {
          setAttachments((prev) => [...prev, { name: read.name, kind: "text", path, text: read.text }]);
        }
      } catch (e) {
        setError(String(e));
      }
    }
  };

  const removeAttachment = (idx: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== idx));
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

  const newChat = async () => {
    if (streaming) return;
    try {
      await invoke("harness_agent_reset");
    } catch {}
    setItems([]);
    setError(null);
  };

  // Delete = rewind: drop the response and the user turn that produced it.
  const lastAssistantIdx = (() => {
    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i];
      if (it.kind === "msg" && it.role === "assistant") return i;
    }
    return -1;
  })();
  const lastUserIdx = (() => {
    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i];
      if (it.kind === "msg" && it.role === "user") return i;
    }
    return -1;
  })();

  const deleteResponse = async (idx: number) => {
    if (idx !== lastAssistantIdx || streaming) return;
    try {
      await invoke("harness_agent_rewind");
      setItems((prev) => prev.slice(0, lastUserIdx));
      setInput((prev) => {
        const removed = items[lastUserIdx];
        return removed && removed.kind === "msg" ? removed.content : prev;
      });
    } catch {}
  };

  // The view is always available: chats are consultable without the server,
  // and sending requires a running server + an active project directory.
  const serverRunning = status.type === "running";
  const canSend = serverRunning && activeProject != null;

  return (
    <div className="flex-1 flex min-h-0">
      {sidebarOpen && (
        <ChatSidebar
          onProjectChanged={() => {
            refreshActiveProject();
            restoreFromBackend();
          }}
          onSessionPicked={() => {
            restoreFromBackend();
          }}
        />
      )}

      <div className="flex-1 flex flex-col min-h-0 min-w-0">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-1.5 border-b border-border">
          <button
            className="text-xs text-gray-500 hover:text-gray-300"
            onClick={() => setSidebarOpen((v) => !v)}
            title={sidebarOpen ? "Hide sidebar" : "Show sidebar"}
          >
            {sidebarOpen ? "◀ Sidebar" : "▶ Sidebar"}
          </button>
          <button
            className="text-xs text-gray-500 hover:text-gray-300"
            onClick={newChat}
            disabled={streaming}
            title="Start a new conversation"
          >
            New chat
          </button>
        </div>

        {/* Availability banners */}
        {!serverRunning && (
          <div className="flex items-center gap-2 px-4 py-1.5 border-b border-border text-[11px] text-gray-500">
            <RefreshCw size={11} className={status.type === "starting" ? "animate-spin" : ""} />
            <span>
              {status.type === "starting"
                ? "Server is starting — the model is loading."
                : "Server is not running — your chats stay available below."}
            </span>
            {status.type !== "starting" && (
              <button
                className="ml-auto text-primary-light hover:underline"
                onClick={() => navigate("/server")}
              >
                Go to Run
              </button>
            )}
          </div>
        )}
        {serverRunning && !activeProject && (
          <div className="flex items-center gap-2 px-4 py-1.5 border-b border-border text-[11px] text-gray-500">
            <FolderOpen size={11} />
            Select or add a working directory (project) in the sidebar to start chatting.
          </div>
        )}

        {/* Messages — select-text re-enables selection (body disables it for the title bar) */}
        <div ref={scrollRef} className="flex-1 overflow-y-auto px-6 py-4 space-y-3 select-text">
          {items.length === 0 && streamText === null && <EmptyState tools={tools} />}
          {items.map((it, i) => {
            if (it.kind === "msg") {
              const isUser = it.role === "user";
              const isLastAssistant = it.role === "assistant" && i === lastAssistantIdx;
              return (
                <div key={i} className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
                  <div className="max-w-[80%]">
                    {isUser && (
                      <div className="text-[10px] text-gray-600 mb-0.5 text-right select-text">
                        {it.time ? formatTime(it.time) : ""}
                      </div>
                    )}
                    {it.reasoning && <ReasoningBlock text={it.reasoning} />}
                    <div
                      className={`rounded px-3 py-2 ${
                        isUser ? "bg-primary/20 text-gray-100 text-sm whitespace-pre-wrap break-words" : "bg-surface-2 text-gray-200"
                      }`}
                    >
                      {isUser ? it.content : <Markdown content={it.content} />}
                    </div>
                    {isUser && it.attachments && it.attachments.length > 0 && (
                      <div className="flex flex-wrap justify-end gap-1 mt-1">
                        {it.attachments.map((name) => (
                          <span key={name} className="text-[10px] text-gray-500 font-mono">📎 {name}</span>
                        ))}
                      </div>
                    )}
                    {!isUser ? (
                      <ResponseFooter
                        model={it.model}
                        tokps={it.tokps}
                        elapsedMs={it.elapsedMs}
                        tokens={it.tokens}
                        onCopy={() => navigator.clipboard.writeText(it.content).catch(() => {})}
                        onDelete={isLastAssistant && !streaming ? () => deleteResponse(i) : undefined}
                      />
                    ) : (
                      <div className="flex items-center justify-end mt-1 px-1 text-[10px] text-gray-600">
                        <button
                          className="inline-flex items-center gap-1 hover:text-gray-300 transition-colors"
                          onClick={() => navigator.clipboard.writeText(it.content).catch(() => {})}
                          title="Copy message"
                        >
                          <Copy size={10} /> Copy
                        </button>
                      </div>
                    )}
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
        {reasoningText !== null && (
          <div className="flex justify-start">
            <div className="max-w-[80%] w-full">
              <ReasoningBlock
                text={reasoningText}
                streaming
                open={reasoningOpen}
                onToggle={() => setReasoningOpen((v) => !v)}
              />
            </div>
          </div>
        )}
        {streamText !== null && (
          <div className="flex justify-start">
            <div className="max-w-[80%] rounded px-3 py-2 bg-surface-2 text-gray-200 select-text">
              <Markdown content={streamText} />
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

          {/* Run status: loading (model warming up) / thinking (agent reasoning) */}
          {streaming && (
            <div className="px-6 pb-1 flex items-center gap-1.5 text-[11px] text-gray-500 select-none">
              {runStatus === "loading" ? (
                <>
                  <RefreshCw size={11} className="animate-spin" />
                  <span>Loading model… your message is queued and will be answered.</span>
                </>
              ) : (
                <>
                  <span className="w-1.5 h-1.5 rounded-full bg-primary-light animate-pulse" />
                  <span>Thinking…</span>
                </>
              )}
            </div>
          )}

          {/* Input */}
        <div className="border-t border-border p-3">
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5 mb-2">
              {attachments.map((a, i) => (
                <div
                  key={`${a.name}-${i}`}
                  className="flex items-center gap-1.5 rounded border border-border bg-surface-2 pl-1 pr-1.5 py-0.5 text-[11px] text-gray-300"
                  title={a.path}
                >
                  {a.kind === "image" && a.preview ? (
                    <img src={a.preview} alt={a.name} className="w-6 h-6 rounded object-cover" />
                  ) : (
                    <span className="text-gray-500 font-mono">📄</span>
                  )}
                  <span className="max-w-[160px] truncate font-mono">{a.name}</span>
                  <button
                    className="text-gray-600 hover:text-accent-red"
                    onClick={() => removeAttachment(i)}
                    title="Remove attachment"
                  >
                    <X size={11} />
                  </button>
                </div>
              ))}
            </div>
          )}
          <div className="flex items-end gap-2">
            <ContextRing used={contextUsed} total={caps?.context_length ?? null} avgTokps={avgTokps} />
            <button
              className="btn-secondary shrink-0 py-2 px-2.5 mb-0.5"
              onClick={attachFiles}
              disabled={streaming}
              title="Attach files or images"
            >
              <Paperclip size={14} />
            </button>
            <textarea
              className="input flex-1 resize-none h-16 text-sm"
              placeholder={
                !canSend
                  ? activeProject == null
                    ? "Select a project to start chatting…"
                    : "Start the server to start chatting…"
                  : "Send a message…"
              }
              value={input}
              disabled={streaming || !canSend}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  send();
                }
              }}
            />
            {/* Capability badges + reasoning effort, stacked next to Send */}
            <div className="flex flex-col items-center gap-1 shrink-0 pb-0.5">
              <div className="flex items-center gap-1.5 h-4" title="Model capabilities">
                {caps?.vision && (
                  <span title="Model supports vision (image input — attach support coming soon)">
                    <Eye size={13} className="text-accent-blue" />
                  </span>
                )}
                {caps?.reasoning && (
                  <span title="Model supports reasoning (thinking)">
                    <Brain size={13} className="text-primary-light" />
                  </span>
                )}
              </div>
              <select
                className="input py-1 px-1 text-[10px] w-20"
                value={reasoningEffort}
                onChange={(e) => setReasoningEffort(e.target.value)}
                title="Reasoning effort (depends on model support)"
                disabled={streaming}
              >
                {REASONING_OPTIONS.map((o) => (
                  <option key={o.value || "default"} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
            {streaming ? (
              <button className="btn-danger shrink-0" onClick={() => invoke("harness_agent_abort").catch(() => {})} title="Stop">
                <Square size={13} />
                Stop
              </button>
            ) : (
              <button
                className="btn-primary shrink-0"
                onClick={send}
                disabled={(!input.trim() && attachments.length === 0) || !canSend}
                title={
                  !canSend
                    ? "Needs a running server and an active project"
                    : "Send"
                }
              >
                <ArrowUp size={14} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

interface ToolListing {
  name: string;
  description: string;
  approval: string;
}

function EmptyState({ tools }: { tools: ToolListing[] | null }) {
  return (
    <div className="h-full flex flex-col items-center justify-center gap-3 text-center">
      <p className="text-base font-semibold text-gray-200">Catapult Chat</p>
      <p className="text-sm text-gray-500 max-w-md">
        The agent can use these sandboxed tools in the project directory:
      </p>
      {tools && tools.length > 0 && (
        <div className="flex flex-col gap-1.5 max-w-lg text-left">
          {tools.map((t) => (
            <div key={t.name} className="flex items-start gap-2 rounded border border-border bg-surface-2 px-2.5 py-1.5">
              <Wrench size={11} className="text-gray-500 shrink-0 mt-0.5" />
              <div className="min-w-0">
                <p className="text-xs">
                  <span className="text-gray-300 font-medium font-mono">{t.name}</span>
                  <span className={`ml-2 text-[10px] ${t.approval === "auto" ? "text-accent-green" : "text-accent-yellow"}`}>
                    {t.approval}
                  </span>
                </p>
                <p className="text-[11px] text-gray-500 leading-snug">{t.description}</p>
              </div>
            </div>
          ))}
        </div>
      )}
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
    const refresh = () => {
      invoke<{ harness_chat: boolean }>("get_config")
        .then((c) => setHarnessChat(c.harness_chat ?? true))
        .catch(() => setHarnessChat(true));
    };
    refresh();
    // The options menu toggles this live — refresh without a restart.
    window.addEventListener("catapult-settings", refresh);
    return () => window.removeEventListener("catapult-settings", refresh);
  }, []);

  if (harnessChat === false) {
    return <WebUIChat />;
  }
  return <HarnessChat />;
}
