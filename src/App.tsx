import { useEffect, useState } from "react";
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import Layout from "./components/Layout";
import Dashboard from "./pages/Dashboard";
import Runtime from "./pages/Runtime";
import Models from "./pages/Models";
import Tools from "./pages/Tools";
import Server from "./pages/Server";
import Api from "./pages/Api";
import Chat from "./pages/Chat";
import Bench from "./pages/Bench";
import Wizard from "./pages/Wizard";
import type { AppConfig } from "./types";

function AppRedirect() {
  const [target, setTarget] = useState<string | null>(null);

  useEffect(() => {
    invoke<AppConfig>("get_config")
      .then((cfg) => setTarget(cfg.wizard_completed ? "/dashboard" : "/wizard"))
      .catch(() => setTarget("/dashboard"));
  }, []);

  if (!target) return null;
  return <Navigate to={target} replace />;
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/wizard" element={<Wizard />} />
        <Route path="/" element={<Layout />}>
          <Route index element={<AppRedirect />} />
          <Route path="dashboard" element={<Dashboard />} />
          <Route path="runtime" element={<Runtime />} />
          <Route path="models" element={<Models />} />
          <Route path="tools" element={<Tools />} />
          <Route path="server" element={<Server />} />
          <Route path="bench" element={<Bench />} />
          <Route path="api" element={<Api />} />
          <Route path="chat" element={<Chat />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
