import { useSyncExternalStore } from "react";

let currentUrl = "";
const listeners = new Set<() => void>();

export function setChatUrl(url: string): void {
  if (currentUrl === url) return;
  currentUrl = url;
  listeners.forEach((l) => l());
}

export function useChatUrl(): string {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => {
        listeners.delete(cb);
      };
    },
    () => currentUrl,
    () => currentUrl,
  );
}
