/** This plugin owns focus subscriptions, polling, deduplication and pending
 * reads. It renders no UI and never receives raw text or a general native API. */
import { useEffect, useRef } from "react";
import { createTranslator } from "../../i18n";
import type { LauncherSurfaceProps } from "../../productPlugins/surfaceTypes";

export function ClipboardDiscoverySurface({
  host,
  lifetime,
}: LauncherSurfaceProps) {
  const channel = host.clipboard;
  const connection = channel?.connection;
  const enabled = channel?.enabled ?? false;
  const available = channel?.available ?? false;
  const context = channel?.context;
  const identity = useRef({
    connection,
    enabled,
    available,
    context,
    lifetime,
  });
  if (
    identity.current.connection !== connection ||
    identity.current.enabled !== enabled ||
    identity.current.available !== available ||
    identity.current.context !== context ||
    identity.current.lifetime !== lifetime
  )
    identity.current = { connection, enabled, available, context, lifetime };
  const scope = identity.current;
  const tr = createTranslator(
    channel?.localization ?? host.announcements.localization,
  );
  const latest = useRef({ scope, notify: channel?.notify, tr });
  latest.current = { scope, notify: channel?.notify, tr };
  const pending = useRef(false),
    lastUrl = useRef<string | null>(null);
  useEffect(() => {
    if (!enabled || !available || !connection || !lifetime.isActive()) {
      lastUrl.current = null;
      return;
    }
    let active = true;
    const allowed = () =>
      active &&
      latest.current.scope === scope &&
      lifetime.isActive() &&
      connection.canNavigate() &&
      connection.isFocused();
    async function read() {
      if (!allowed() || pending.current) return;
      pending.current = true;
      try {
        const link = await connection!.read();
        if (!allowed() || !link || link.url === lastUrl.current) return;
        // Keep the consumed URL across navigation contexts. Opening a resource
        // must not cause its new context to rediscover and reopen the same URL.
        lastUrl.current = link.url;
        connection!.open(link);
      } catch (error) {
        if (allowed())
          latest.current.notify?.(latest.current.tr.serviceError(error));
      } finally {
        pending.current = false;
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
  }, [scope]);
  return null;
}
