/** The announcement plugin owns presentation, requests and activation state.
 * Its Rust provider owns caching; retiring this component never cancels another
 * consumer's admitted read or starts the Code/Agent runtime. */
import { useEffect, useRef, useState } from "react";
import { createTranslator } from "../../i18n";
import type { LauncherAnnouncementView } from "../../launcherDiscoveryTypes";
import type { LauncherSurfaceProps } from "../../productPlugins/surfaceTypes";

export function AnnouncementSurface({ host, lifetime }: LauncherSurfaceProps) {
  const { connection, available, scope, policy, localization, notify } =
    host.announcements;
  const tr = createTranslator(localization);
  // An activation/connection has its own identity even after a policy returns
  // to a previously used value. Saved handlers cannot cross that boundary.
  const identity = useRef({ connection, available, scope, policy, lifetime });
  if (
    identity.current.connection !== connection ||
    identity.current.available !== available ||
    identity.current.scope !== scope ||
    identity.current.policy !== policy ||
    identity.current.lifetime !== lifetime
  )
    identity.current = { connection, available, scope, policy, lifetime };
  const owner = identity.current;
  const latest = useRef({ owner, available, notify, tr });
  latest.current = { owner, available, notify, tr };
  type State = {
    owner: typeof owner;
    announcements: LauncherAnnouncementView | null;
    loading: boolean;
    error: string;
  };
  const [state, setState] = useState<State>({
    owner,
    announcements: null,
    loading: false,
    error: "",
  });
  // Never display an old policy's data for the frame before effects reconcile.
  const { announcements, loading, error } =
    state.owner === owner
      ? state
      : { announcements: null, loading: false, error: "" };
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
      !live.current ||
      !latest.current.available ||
      owner !== latest.current.owner ||
      !lifetime.isActive() ||
      scope === "none" ||
      announcementPending.current
    )
      return;
    const captured = owner,
      epoch = ++announcementEpoch.current;
    announcementPending.current = true;
    setState((old) => ({
      owner,
      announcements: old.owner === owner ? old.announcements : null,
      loading: true,
      error: "",
    }));
    try {
      const result = await connection.read(refresh);
      if (
        live.current &&
        epoch === announcementEpoch.current &&
        captured === latest.current.owner &&
        lifetime.isActive()
      )
        setState((old) =>
          old.owner === captured ? { ...old, announcements: result } : old,
        );
    } catch (error) {
      if (
        live.current &&
        epoch === announcementEpoch.current &&
        captured === latest.current.owner &&
        lifetime.isActive()
      )
        setState((old) =>
          old.owner === captured ? { ...old, error: String(error) } : old,
        );
    } finally {
      if (epoch === announcementEpoch.current) {
        announcementPending.current = false;
        if (
          live.current &&
          captured === latest.current.owner &&
          lifetime.isActive()
        )
          setState((old) =>
            old.owner === captured ? { ...old, loading: false } : old,
          );
      }
    }
  }
  useEffect(() => {
    announcementEpoch.current++;
    announcementPending.current = false;
    setState({ owner, announcements: null, error: "", loading: false });
    if (available && scope !== "none" && lifetime.isActive())
      void readAnnouncements();
    return () => {
      announcementEpoch.current++;
    };
  }, [connection, available, scope, policy, lifetime]);
  const announcementUi =
    available && lifetime.isActive() && scope !== "none" ? (
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
                    if (
                      !live.current ||
                      owner !== latest.current.owner ||
                      !lifetime.isActive()
                    )
                      return;
                    void connection.openRelease(item.url).catch((error) => {
                      if (
                        live.current &&
                        owner === latest.current.owner &&
                        lifetime.isActive()
                      )
                        latest.current.notify(
                          latest.current.tr.serviceError(error),
                        );
                    });
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
  return announcementUi;
}
