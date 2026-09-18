import { NavLink, Outlet, useNavigate, useLocation } from "react-router-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";

import { useEffect, useRef, useState } from "react";
import {
  LayoutDashboard,
  Download,
  Database,
  Wrench,
  Play,
  MessageSquare,
  FlaskConical,
  Settings,
} from "lucide-react";
import { clsx } from "clsx";
import type { LucideIcon } from "lucide-react";
import CatapultIcon from "./CatapultIcon";
import WindowControls from "./WindowControls";
import OptionsPanel from "./OptionsPanel";
import Chat from "../pages/Chat";
import type { ServerStatus } from "../types";
import {
  getQuickBenchEnabled,
  getUpdateState,
  loadQuickBenchEnabled,
  subscribeQuickBench,
  subscribeUpdateState,
  checkForAppUpdateOnStartup,
} from "../utils/appSettings";

const navItems: { to: string; label: string; icon: LucideIcon; quickBench?: boolean }[] = [
  { to: "/dashboard", label: "Dashboard", icon: LayoutDashboard },
  { to: "/runtime", label: "Runtime", icon: Download },
  { to: "/models", label: "Models", icon: Database },
  { to: "/tools", label: "Tools", icon: Wrench },
  { to: "/server", label: "Run", icon: Play },
  { to: "/bench", label: "Bench", icon: FlaskConical, quickBench: true },
  { to: "/chat", label: "Chat", icon: MessageSquare },
];

function VersionInfo() {
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    getVersion().then(setVersion);
  }, []);

  if (!version) return null;

  return (
    <div className="relative z-10 flex items-center gap-1.5 ml-2">
      <span className="text-xs text-gray-500 select-none tabular-nums">v{version}</span>
    </div>
  );
}

export default function Layout() {
  const navigate = useNavigate();
  const location = useLocation();
  const onDashboard = location.pathname === "/dashboard";
  const [optionsOpen, setOptionsOpen] = useState(false);
  const optionsBtnRef = useRef<HTMLButtonElement | null>(null);
  const [quickBench, setQuickBench] = useState(getQuickBenchEnabled());
  const [updateState, setUpdateStateLocal] = useState(getUpdateState());
  const [serverRunning, setServerRunning] = useState(false);

  useEffect(() => {
    loadQuickBenchEnabled();
    checkForAppUpdateOnStartup();
    const unsubQuick = subscribeQuickBench(setQuickBench);
    const unsubUpdate = subscribeUpdateState(setUpdateStateLocal);
    // Nav highlight: server state feeds the Run/Chat icons.
    const poll = () => {
      invoke<ServerStatus>("get_server_status")
        .then((s) => setServerRunning(s.type === "running" || s.type === "starting"))
        .catch(() => setServerRunning(false));
    };
    poll();
    const id = setInterval(poll, 2000);
    return () => {
      unsubQuick();
      unsubUpdate();
      clearInterval(id);
    };
  }, []);

  // Close on outside click; gear button toggles so exclude it.
  useEffect(() => {
    if (!optionsOpen) return;
    const onDocMouseDown = (e: MouseEvent) => {
      const target = e.target as Node;
      const inPanel = target instanceof Element && target.closest("[data-options-panel]");
      if (!inPanel && !(optionsBtnRef.current && optionsBtnRef.current.contains(target))) {
        setOptionsOpen(false);
      }
    };
    const t = setTimeout(() => document.addEventListener("mousedown", onDocMouseDown), 0);
    return () => {
      clearTimeout(t);
      document.removeEventListener("mousedown", onDocMouseDown);
    };
  }, [optionsOpen]);

  return (
    <div className="relative flex flex-col h-full bg-surface-0">
      <div
        className="relative flex items-center h-11 pl-3 pr-0 border-b border-primary/25 shrink-0 bg-primary/8"
      >
        {/* Drag region fills the bar behind buttons so empty areas still drag. */}
        <div
          className="absolute inset-0"
          onMouseDown={(e) => {
            if (e.button === 0) getCurrentWindow().startDragging();
          }}
          onDoubleClick={() => getCurrentWindow().toggleMaximize()}
        />

        <button
          onClick={() => navigate("/dashboard")}
          disabled={onDashboard}
          className={`relative z-10 flex items-center gap-2 px-1.5 py-1 -ml-1 rounded transition-colors ${
            onDashboard
              ? "cursor-default"
              : "hover:bg-primary/15 active:bg-primary/25"
          }`}
          title={onDashboard ? "Catapult" : "Back to Dashboard"}
        >
          <CatapultIcon size={22} className="text-primary-light" />
          <span className="text-sm font-semibold text-gray-200 tracking-tight select-none">
            Catapult
          </span>
        </button>

        <VersionInfo />

        <div className="relative z-10 w-px h-5 bg-primary/20 mx-3" />

        <nav className="relative z-10 flex items-center gap-0.5">
          {navItems
            .filter((item) => !item.quickBench || quickBench)
            .map(({ to, label, icon: Icon }) => {
              // Run turns green with the server; Chat lights up white when
              // the server runs (chats are consultable, sending needs it).
              const highlight =
                to === "/server" && serverRunning
                  ? "text-accent-green"
                  : to === "/chat" && serverRunning
                    ? "text-gray-100"
                    : null;
              return (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                clsx(
                  "flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded transition-colors",
                  isActive
                    ? "bg-primary/20 text-primary-light"
                    : highlight
                      ? `${highlight} hover:bg-primary/10`
                      : "text-gray-400 hover:text-gray-200 hover:bg-primary/10"
                )
              }
            >
              <Icon size={13} />
              {label}
            </NavLink>
              );
            })}
        </nav>

        <div className="relative z-10 ml-auto flex items-center">
          <button
            ref={optionsBtnRef}
            onClick={() => setOptionsOpen((v) => !v)}
            className={clsx(
              "relative w-8 h-8 mr-1 flex items-center justify-center rounded transition-colors",
              optionsOpen
                ? "bg-primary/20 text-primary-light"
                : "text-gray-400 hover:text-gray-200 hover:bg-primary/10"
            )}
            title="Settings"
          >
            <Settings size={15} />
            {updateState.available && !optionsOpen && (
              <span className="absolute top-1 right-1 w-1.5 h-1.5 rounded-full bg-accent-yellow" />
            )}
          </button>
          <WindowControls />
        </div>
      </div>

      {/* Stays mounted so update checks keep running. */}
      <div data-options-panel>
        <OptionsPanel open={optionsOpen} onClose={() => setOptionsOpen(false)} />
      </div>

      <main className="flex-1 overflow-hidden flex flex-col min-w-0">
        <div style={{ display: location.pathname === "/chat" ? "none" : "flex", flex: 1, overflow: "hidden", flexDirection: "column", minWidth: 0 }}>
          <Outlet />
        </div>
        <div style={{ display: location.pathname === "/chat" ? "flex" : "none", flex: 1, overflow: "hidden", flexDirection: "column", minWidth: 0 }}>
          <Chat />
        </div>
      </main>
    </div>
  );
}
