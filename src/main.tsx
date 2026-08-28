import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import "./styles/globals.css";
import { setThemePreference, initTheme } from "./utils/theme";
import type { AppConfig, AppTheme } from "./types";

// Apply the branded look immediately, then switch to the persisted preference
// before React renders (avoids a wrong-theme flash).
setThemePreference("catapult");
invoke<AppConfig>("get_config")
  .then((cfg) => initTheme(cfg.theme as AppTheme))
  .catch(() => initTheme("system"));

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);