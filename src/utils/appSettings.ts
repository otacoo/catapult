import { invoke } from "@tauri-apps/api/core";
import { check } from "@tauri-apps/plugin-updater";

// Tiny store for app-wide settings that multiple views react to live (nav
// items, Run header buttons). The source of truth is AppConfig; this mirrors
// the values in memory so toggling in the settings window updates instantly.

let quickBenchEnabled = true;
const listeners = new Set<(v: boolean) => void>();

export function getQuickBenchEnabled(): boolean {
  return quickBenchEnabled;
}

export function setQuickBenchEnabled(v: boolean): void {
  if (v === quickBenchEnabled) return;
  quickBenchEnabled = v;
  listeners.forEach((l) => l(v));
}

export function subscribeQuickBench(cb: (v: boolean) => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/** Load the persisted value once (call from Layout on mount). */
export function loadQuickBenchEnabled(): void {
  invoke<{ enable_quick_bench: boolean }>("get_config")
    .then((c) => setQuickBenchEnabled(c.enable_quick_bench))
    .catch(() => {});
}

// ── App update state (checked on app start, badge on the gear icon) ────────

interface UpdateState {
  available: boolean;
  version: string | null;
}

let updateState: UpdateState = { available: false, version: null };
const updateListeners = new Set<(s: UpdateState) => void>();

export function getUpdateState(): UpdateState {
  return updateState;
}

function setUpdateState(next: UpdateState): void {
  updateState = next;
  updateListeners.forEach((l) => l(next));
}

export function subscribeUpdateState(cb: (s: UpdateState) => void): () => void {
  updateListeners.add(cb);
  return () => updateListeners.delete(cb);
}

/** Runs when "check for updates on app start" is enabled (Layout mount). */
export function checkForAppUpdateOnStartup(): void {
  invoke<{ auto_check_updates: boolean }>("get_config")
    .then((c) => {
      if (!c.auto_check_updates) return;
      return check()
        .then((u) => {
          setUpdateState({ available: u != null, version: u?.version ?? null });
        })
        .catch(() => {});
    })
    .catch(() => {});
}
