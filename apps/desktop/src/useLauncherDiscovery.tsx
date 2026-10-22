import { useEffect, useRef, useState } from "react";
import type { Api } from "./types";
import type { LauncherEffect } from "./launcherTypes";
import type { useLauncherPreferences } from "./useLauncherPreferences";
import type {
  LauncherAnnouncementView,
  LauncherClipboardLink,
} from "./launcherDiscoveryTypes";
import { createTranslator } from "./i18n";

const effects: readonly LauncherEffect[] = [
  "announcements",
  "clipboard_detection",
];

/** Releases follow their selected display/network policy. Clipboard reads have
 * a separate opt-in and one pending request; native GTK independently rechecks
 * permission and keyboard focus before releasing an approved resource identity.
 * Navigation changes invalidate late reads. Only approved URLs are deduplicated
 * in memory; no source clipboard text is returned, transmitted or persisted. */
export function useLauncherDiscovery({
  api,
  native,
  enabled,
  pluginEnabled = true,
  launcher,
  onNotify,
  contextKey,
  canNavigate,
  onResource,
}: {
  api: Api;
  native: boolean;
  enabled: boolean;
  pluginEnabled?: boolean;
  launcher: ReturnType<typeof useLauncherPreferences>;
  onNotify: (message: string) => void;
  contextKey: string;
  canNavigate: () => boolean;
  onResource: (link: LauncherClipboardLink) => void;
}) {
  const prefs = launcher.prefs;
  const tr = createTranslator(prefs.localization);
  const available = native && enabled && !!launcher.view.revision;
  const policy = `${pluginEnabled}:${prefs.announcement_mode}:${JSON.stringify(prefs.network)}:${launcher.importEpoch}`;
  const clipboardEnabled =
    available && prefs.management.clipboard_resource_detection;
  // A disable/reenable cycle can return to the same policy string. Give each
  // activation a distinct owner so saved buttons cannot become live again.
  const announcementOwner = useRef({ api, available, policy });
  if (
    announcementOwner.current.api !== api ||
    announcementOwner.current.available !== available ||
    announcementOwner.current.policy !== policy
  )
    announcementOwner.current = { api, available, policy };
  const owner = announcementOwner.current;
  const latest = useRef({
    api,
    available,
    policy,
    owner,
    clipboardEnabled,
    contextKey,
    canNavigate,
    onResource,
    onNotify,
    tr,
  });
  latest.current = {
    api,
    available,
    policy,
    owner,
    clipboardEnabled,
    contextKey,
    canNavigate,
    onResource,
    onNotify,
    tr,
  };
  const [announcements, setAnnouncements] =
    useState<LauncherAnnouncementView | null>(null);
  const [loading, setLoading] = useState(false),
    [error, setError] = useState("");
  const announcementEpoch = useRef(0),
    announcementPending = useRef(false);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      announcementEpoch.current++;
    };
  }, []);
  async function readAnnouncements(refresh = false) {
    if (
      !latest.current.available ||
      !pluginEnabled ||
      owner !== latest.current.owner ||
      prefs.announcement_mode === "none" ||
      announcementPending.current
    )
      return;
    const captured = owner,
      epoch = ++announcementEpoch.current;
    announcementPending.current = true;
    setLoading(true);
    setError("");
    try {
      const result = await api<LauncherAnnouncementView>(
        "launcher_announcements",
        { refresh },
      );
      if (
        live.current &&
        epoch === announcementEpoch.current &&
        captured === latest.current.owner
      )
        setAnnouncements(result);
    } catch (error) {
      if (
        live.current &&
        epoch === announcementEpoch.current &&
        captured === latest.current.owner
      )
        setError(String(error));
    } finally {
      if (epoch === announcementEpoch.current) {
        announcementPending.current = false;
        if (live.current) setLoading(false);
      }
    }
  }
  useEffect(() => {
    announcementEpoch.current++;
    announcementPending.current = false;
    setAnnouncements(null);
    setError("");
    setLoading(false);
    if (available && pluginEnabled && prefs.announcement_mode !== "none")
      void readAnnouncements();
    return () => {
      announcementEpoch.current++;
    };
  }, [api, available, policy]);
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
  const announcementUi =
    available && pluginEnabled && prefs.announcement_mode !== "none" ? (
      <section className="ce-card">
        <h2 className="ce-card-title">{tr.t("discovery.announcements")}</h2>
        <p className="extra-paragraph extra-muted">
          {tr.t("discovery.releaseHelp")}
        </p>
        {loading && (
          <p className="extra-paragraph">{tr.t("discovery.loading")}</p>
        )}
        {announcements && (
          <>
            {announcements.state === "empty" && (
              <p className="extra-paragraph">{tr.t("discovery.empty")}</p>
            )}
            {announcements.state === "rateLimited" && (
              <p className="extra-paragraph">
                {tr.t("discovery.rateLimited")}
                {announcements.retryAt &&
                  tr.t("discovery.retryAt", {
                    date: tr.formatDate(announcements.retryAt * 1000),
                  })}
              </p>
            )}
            {announcements.state === "unavailable" && (
              <p className="extra-paragraph">{tr.t("discovery.unavailable")}</p>
            )}
            {announcements.cached && (
              <p className="extra-paragraph extra-muted">
                {tr.t("discovery.cached")}
              </p>
            )}
            {announcements.truncated && (
              <p className="extra-paragraph extra-muted">
                {tr.t("discovery.truncated")}
              </p>
            )}
            {announcements.message && (
              <p className="extra-paragraph extra-muted">
                {tr.t("common.original", { message: announcements.message })}
              </p>
            )}
            {announcements.items.map((item) => (
              <div className="extra-paragraph" key={item.id}>
                <h3>{item.title}</h3>
                <p>
                  {item.prerelease
                    ? tr.t("discovery.preview")
                    : tr.t("discovery.stable")}
                  {item.publishedAt
                    ? ` · ${tr.formatDate(item.publishedAt)}`
                    : ""}
                </p>
                <p style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
                  {item.text}
                </p>
                <button
                  className="ce-button"
                  onClick={() => {
                    if (!live.current || owner !== latest.current.owner) return;
                    void api("ui_open_link", { url: item.url }).catch(
                      (error) => {
                        if (live.current && owner === latest.current.owner)
                          latest.current.onNotify(
                            latest.current.tr.serviceError(error),
                          );
                      },
                    );
                  }}
                >
                  {tr.t("discovery.openRelease")}
                </button>
              </div>
            ))}
          </>
        )}
        {error && (
          <p className="extra-paragraph rd-name-error" role="alert">
            {tr.serviceError(error)}
          </p>
        )}
        <div className="ce-actions">
          <button
            className="ce-button"
            disabled={loading}
            onClick={() => void readAnnouncements(true)}
          >
            {tr.t("discovery.refresh")}
          </button>
        </div>
      </section>
    ) : null;
  return { effects: enabled ? effects : [], announcementUi };
}
