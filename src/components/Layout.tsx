import { NavLink, Outlet, useNavigate, useLocation } from "react-router-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";

import { useEffect, useState } from "react";
import {
  LayoutDashboard,
  Download,
  Database,
  Wrench,
  Play,
  MessageSquare,
  Plug,
} from "lucide-react";
import { clsx } from "clsx";
import CatapultIcon from "./CatapultIcon";
import WindowControls from "./WindowControls";

const navItems = [
  { to: "/dashboard", label: "Dashboard", icon: LayoutDashboard },
  { to: "/runtime", label: "Runtime", icon: Download },
  { to: "/models", label: "Models", icon: Database },
  { to: "/tools", label: "Tools", icon: Wrench },
  { to: "/server", label: "Run", icon: Play },
  { to: "/api", label: "API", icon: Plug },
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

  return (
    <div className="flex flex-col h-full bg-surface-0">
      {/* Title bar — custom, replaces OS decorations */}
      <div
        className="relative flex items-center h-11 px-3 border-b border-primary/25 shrink-0 bg-primary/8"
      >
        {/* Drag region — fills entire title bar behind interactive elements */}
        <div
          className="absolute inset-0"
          onMouseDown={(e) => {
            if (e.button === 0) getCurrentWindow().startDragging();
          }}
          onDoubleClick={() => getCurrentWindow().toggleMaximize()}
        />

        {/* Logo / home */}
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

        {/* Version + update indicator */}
        <VersionInfo />

        {/* Separator */}
        <div className="relative z-10 w-px h-5 bg-primary/20 mx-3" />

        {/* Nav */}
        <nav className="relative z-10 flex items-center gap-0.5">
          {navItems.map(({ to, label, icon: Icon }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                clsx(
                  "flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded transition-colors",
                  isActive
                    ? "bg-primary/20 text-primary-light"
                    : "text-gray-400 hover:text-gray-200 hover:bg-primary/10"
                )
              }
            >
              <Icon size={13} />
              {label}
            </NavLink>
          ))}
        </nav>

        {/* Window controls */}
        <div className="relative z-10 ml-auto">
          <WindowControls />
        </div>
      </div>

      {/* Main */}
      <main className="flex-1 overflow-hidden flex flex-col min-w-0">
        <Outlet />
      </main>
    </div>
  );
}
