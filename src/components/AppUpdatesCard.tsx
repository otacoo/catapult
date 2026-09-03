import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  RefreshCw,
  ArrowUpCircle,
  Download,
  ExternalLink,
} from "lucide-react";
import type { AppConfig } from "../types";
import Toggle from "./Toggle";

export default function AppUpdatesCard() {
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [appConfig, setAppConfig] = useState<AppConfig | null>(null);
  const [updateAvailable, setUpdateAvailable] = useState<boolean>(false);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [pendingUpdate, setPendingUpdate] = useState<Update | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [checkedUpdate, setCheckedUpdate] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [downloadPercent, setDownloadPercent] = useState<number | null>(null);
  const [installNote, setInstallNote] = useState<string | null>(null);

  useEffect(() => {
    getVersion().then(setAppVersion);
    invoke<AppConfig>("get_config").then(setAppConfig).catch(() => {});
  }, []);

  useEffect(() => {
    if (appConfig?.auto_check_updates) {
      checkForUpdate();
    }
  }, [appConfig?.auto_check_updates]);

  const checkForUpdate = async () => {
    setCheckingUpdate(true);
    setCheckedUpdate(false);
    try {
      const update = await check();
      setUpdateAvailable(update != null);
      setUpdateVersion(update?.version ?? null);
      setPendingUpdate(update);
    } catch {
      // noop
    } finally {
      setCheckingUpdate(false);
      setCheckedUpdate(true);
    }
  };

  const installUpdate = async () => {
    if (!pendingUpdate) return;
    setUpdating(true);
    setDownloadPercent(null);
    setInstallNote(null);
    try {
      let total = 0;
      let downloaded = 0;
      await pendingUpdate.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? 0;
            downloaded = 0;
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            if (total > 0) {
              setDownloadPercent(Math.min(100, (downloaded / total) * 100));
            }
            break;
          case "Finished":
            setDownloadPercent(100);
            break;
        }
      });
      setInstallNote("Installer launched — Catapult will close and reopen.");
      await relaunch();
    } catch {
      setUpdating(false);
    }
  };

  const setAutoCheckUpdates = async (enabled: boolean) => {
    try {
      await invoke("set_auto_check_updates", { enabled });
      const cfg = await invoke<AppConfig>("get_config");
      setAppConfig(cfg);
    } catch {}
  };

  const openReleasesPage = async () => {
    try {
      await openUrl("https://github.com/otacoo/catapult/releases");
    } catch {}
  };

  return (
    <div className="card">
      <div className="flex items-center justify-between mb-3">
        <h2 className="section-title mb-0">App Updates</h2>
        {appVersion && (
          <span className="text-xs text-gray-500 tabular-nums">v{appVersion}</span>
        )}
      </div>
      <div className="space-y-3">
        <Toggle label="Check for updates on app start"
          checked={appConfig?.auto_check_updates ?? false}
          onChange={setAutoCheckUpdates} />
        <div className="flex flex-wrap items-center gap-3">
          <button
            className="btn-secondary text-xs"
            onClick={checkForUpdate}
            disabled={checkingUpdate || updating}
          >
            <RefreshCw size={13} className={checkingUpdate ? "animate-spin" : ""} />
            Check now
          </button>
          {checkingUpdate && (
            <span className="text-xs text-gray-500">Checking…</span>
          )}
          {checkedUpdate && updateAvailable && !updating && (
            <>
              <button
                className="btn-primary text-xs"
                onClick={installUpdate}
              >
                <Download size={13} />
                Update to v{updateVersion}
              </button>
              <button
                className="text-xs text-gray-500 hover:text-gray-300 inline-flex items-center gap-1"
                onClick={openReleasesPage}
                title="Open the GitHub releases page"
              >
                <ExternalLink size={12} />
                Releases
              </button>
            </>
          )}
          {checkedUpdate && updateAvailable && updating && (
            <span className="flex items-center gap-2 text-xs text-gray-400">
              <ArrowUpCircle size={13} className="text-primary-light" />
              {downloadPercent != null
                ? `Downloading update… ${downloadPercent.toFixed(0)}%`
                : "Downloading update…"}
            </span>
          )}
          {checkedUpdate && !updateAvailable && !updating && (
            <span className="text-xs text-accent-green">Up to date</span>
          )}
        </div>
        {updating && downloadPercent != null && (
          <div className="w-full h-1.5 bg-surface-3 rounded overflow-hidden">
            <div
              className="h-full bg-primary transition-all duration-150"
              style={{ width: `${downloadPercent}%` }}
            />
          </div>
        )}
        {installNote && (
          <p className="text-xs text-accent-yellow">{installNote}</p>
        )}
      </div>
    </div>
  );
}
