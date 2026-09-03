import { invoke } from "@tauri-apps/api/core";

// Tiny store for app-wide settings that multiple views react to live (nav
// items, Run header buttons). The source of truth is AppConfig; this mirrors
// the values in memory so toggling in the options menu updates instantly.

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
