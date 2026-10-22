/** Adapt the existing launcher services to narrow component ports. Components
 * receive no project controller, account state or general native command API. */
import { useMemo } from "react";
import type { Api } from "../types";
import type { useLauncherPreferences } from "../useLauncherPreferences";
import type { LauncherSurfaceHost } from "./surfaceTypes";

export function useLauncherSurfaceHost(
  api: Api,
  native: boolean,
  launcher: ReturnType<typeof useLauncherPreferences>,
  notify: (message: string) => void,
): LauncherSurfaceHost {
  const connection = useMemo(
    () => ({
      read: (refresh: boolean) =>
        api<import("../launcherDiscoveryTypes").LauncherAnnouncementView>(
          "launcher_announcements",
          { refresh },
        ),
      openRelease: (url: string) => api("ui_open_link", { url }),
    }),
    [api],
  );
  return {
    announcements: {
      connection,
      available: native && !!launcher.view.revision,
      scope: launcher.prefs.announcement_mode,
      policy: `${launcher.prefs.announcement_mode}:${JSON.stringify(launcher.prefs.network)}:${launcher.importEpoch}`,
      localization: launcher.prefs.localization,
      notify,
    },
  };
}
