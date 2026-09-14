import { invoke } from "@tauri-apps/api/core";
import type { AppConfig } from "../types";

export type NotificationKind = "agent" | "permissions" | "errors";

const FILES: Record<NotificationKind, string> = {
  agent: "/sounds/finished.mp3",
  permissions: "/sounds/ask.mp3",
  errors: "/sounds/error.mp3",
};

// Nudge the autoplay policy on first interaction so later programmatic
// plays (agent finish, approvals) are allowed. Failures stay silent.
let unlocked = false;
function unlock() {
  if (unlocked) return;
  unlocked = true;
  try {
    const a = new Audio(FILES.agent);
    a.muted = true;
    void a.play().catch(() => {});
  } catch {}
}
if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", unlock, { once: true });
  window.addEventListener("keydown", unlock, { once: true });
}

/** Play a notification sound if its Settings toggle is on. Never throws. */
export async function playNotificationSound(kind: NotificationKind): Promise<void> {
  try {
    const cfg = await invoke<AppConfig>("get_config");
    const enabled =
      kind === "agent"
        ? cfg.sound_agent !== false
        : kind === "permissions"
          ? cfg.sound_permissions !== false
          : cfg.sound_errors !== false;
    if (!enabled) return;
    const audio = new Audio(FILES[kind]);
    audio.volume = 0.8;
    await audio.play();
  } catch {}
}
