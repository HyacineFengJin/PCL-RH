/** Narrow clipboard port. A navigation context owns its replies and callbacks;
 * saved actions cannot cross a preference, availability or host replacement. */
import { useEffect, useMemo, useRef } from "react";
import type { Api } from "../types";
import type { LauncherClipboardLink } from "../launcherDiscoveryTypes";
import type { LauncherSurfaceHost } from "./surfaceTypes";

export type ClipboardDiscoveryConnection = {
  read(): Promise<LauncherClipboardLink | null>;
  canNavigate(): boolean;
  isFocused(): boolean;
  open(link: LauncherClipboardLink): void;
};
export function createClipboardDiscoveryConnection(binding: {
  api: Api;
  isCurrent(): boolean;
  canNavigate(): boolean;
  isFocused(): boolean;
  onResource(link: LauncherClipboardLink): void;
}): ClipboardDiscoveryConnection {
  const allowed = () =>
    binding.isCurrent() && binding.canNavigate() && binding.isFocused();
  return {
    async read() {
      if (!allowed()) return null;
      const link = await binding.api<LauncherClipboardLink | null>(
        "launcher_clipboard_link",
      );
      return allowed() ? link : null;
    },
    canNavigate: () => binding.isCurrent() && binding.canNavigate(),
    isFocused: () => binding.isCurrent() && binding.isFocused(),
    open(link) {
      if (allowed()) binding.onResource(link);
    },
  };
}

export function useClipboardDiscoveryHost(
  host: LauncherSurfaceHost,
  binding: {
    api: Api;
    native: boolean;
    enabled: boolean;
    contextKey: string;
    canNavigate(): boolean;
    onResource(link: LauncherClipboardLink): void;
  },
): LauncherSurfaceHost {
  const { api, native, enabled, contextKey } = binding;
  const owner = useMemo(() => ({}), [api, native, enabled, contextKey]);
  const current = useRef({ owner, binding, live: true });
  current.current = { owner, binding, live: current.current.live };
  useEffect(() => {
    current.current.live = true;
    return () => {
      current.current.live = false;
    };
  }, []);
  const connection = useMemo(
    () =>
      createClipboardDiscoveryConnection({
        api,
        isCurrent: () =>
          current.current.live &&
          current.current.owner === owner &&
          current.current.binding.native &&
          current.current.binding.enabled,
        canNavigate: () => current.current.binding.canNavigate(),
        isFocused: () =>
          typeof document !== "undefined" &&
          (typeof document.hasFocus !== "function" || document.hasFocus()),
        onResource: (link) => current.current.binding.onResource(link),
      }),
    [api, owner],
  );
  return {
    ...host,
    clipboard: {
      connection,
      available: native,
      enabled,
      context: contextKey,
      localization: host.announcements.localization,
      notify: host.announcements.notify,
    },
  };
}
