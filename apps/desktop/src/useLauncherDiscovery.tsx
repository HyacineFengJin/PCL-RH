import { useEffect, useRef } from "react";
import type { Api } from "./types";
import type { LauncherEffect } from "./launcherTypes";
import type { useLauncherPreferences } from "./useLauncherPreferences";
import type { LauncherClipboardLink } from "./launcherDiscoveryTypes";
import { createTranslator } from "./i18n";

const effects: readonly LauncherEffect[] = ["clipboard_detection"];

/** Focused, opted-in clipboard discovery is independent of the announcement
 * plugin. Only approved resource identities leave the native clipboard gate. */
export function useLauncherDiscovery({
  api,
  native,
  enabled,
  launcher,
  onNotify,
  contextKey,
  canNavigate,
  onResource,
}: {
  api: Api;
  native: boolean;
  enabled: boolean;
  launcher: ReturnType<typeof useLauncherPreferences>;
  onNotify: (message: string) => void;
  contextKey: string;
  canNavigate: () => boolean;
  onResource: (link: LauncherClipboardLink) => void;
}) {
  const tr = createTranslator(launcher.prefs.localization);
  const clipboardEnabled =
    native &&
    enabled &&
    !!launcher.view.revision &&
    launcher.prefs.management.clipboard_resource_detection;
  const latest = useRef({
    clipboardEnabled,
    contextKey,
    canNavigate,
    onResource,
    onNotify,
    tr,
  });
  latest.current = {
    clipboardEnabled,
    contextKey,
    canNavigate,
    onResource,
    onNotify,
    tr,
  };
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const clipboardPending = useRef(false),
    lastUrl = useRef<string | null>(null);
  useEffect(() => {
    if (!clipboardEnabled) {
      lastUrl.current = null;
      return;
    }
    let active = true;
    async function read() {
      const state = latest.current;
      if (
        !active ||
        !state.clipboardEnabled ||
        clipboardPending.current ||
        !state.canNavigate() ||
        (typeof document.hasFocus === "function" && !document.hasFocus())
      )
        return;
      clipboardPending.current = true;
      const captured = state.contextKey;
      try {
        const link = await api<LauncherClipboardLink | null>(
          "launcher_clipboard_link",
        );
        const current = latest.current;
        if (
          !active ||
          !live.current ||
          !current.clipboardEnabled ||
          current.contextKey !== captured ||
          !current.canNavigate() ||
          (typeof document.hasFocus === "function" && !document.hasFocus()) ||
          !link ||
          link.url === lastUrl.current
        )
          return;
        lastUrl.current = link.url;
        current.onResource(link);
      } catch (error) {
        if (
          active &&
          latest.current.clipboardEnabled &&
          latest.current.contextKey === captured
        )
          latest.current.onNotify(latest.current.tr.serviceError(error));
      } finally {
        clipboardPending.current = false;
      }
    }
    void read();
    window.addEventListener("focus", read);
    const timer = window.setInterval(read, 5000);
    return () => {
      active = false;
      window.removeEventListener("focus", read);
      window.clearInterval(timer);
    };
  }, [api, clipboardEnabled]);
  return { effects: enabled ? effects : [] };
}
