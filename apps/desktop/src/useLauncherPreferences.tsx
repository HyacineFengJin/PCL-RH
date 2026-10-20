import { t, formatNumber } from "./i18n";
import { createContext, useEffect, useRef, useState } from "react";
import type { Api } from "./types";
import type { ResourceBrowseRequest } from "./resourceBrowse";
import {
  defaultLauncherPreferenceView,
  type LauncherPreferenceView,
  type LauncherPreferencePatch,
  type LauncherMenuId,
  type LauncherAction,
  type LauncherEffect,
} from "./launcherTypes";
import { InstanceOperationDialog } from "./instanceOperationUi";
import { animateLauncher } from "./launcherAnimation";
import { subscribeLauncherAppearanceApplied } from "./extensionEvents";
import "./launcher-theme.css";

export const LauncherNavigationContext = createContext({
  isHidden: (_id: LauncherMenuId): boolean => false,
  modDisplayStyle: "metadata" as "metadata" | "file_name",
  standaloneSaveAvailable: false,
  browseResources: null as ((request: ResourceBrowseRequest) => void) | null,
});
const effects: readonly LauncherEffect[] = [
  "appearance",
  "window_lock",
  "launch_tips",
  "navigation",
  "animations",
  "log_limit",
  "hardware_acceleration",
  "auto_select_installed",
  "settings_transfer",
  "logs",
  "artificial_delay",
  "localization",
  "download_policy",
  "download_copy",
  "mod_display_style",
  "save_policy",
];
type ImportPreview = {
  token: string;
  revision: string;
  preferences: LauncherPreferenceView["preferences"];
};
type Confirmation = { kind: "import"; preview: ImportPreview; error: string };

/** Own the launcher revision independently of root bootstrap reads. One native
 * operation at a time prevents rapid controls or a late chooser from adopting
 * an older revision after another edit has committed. Navigation never owns it. */
export function useLauncherPreferences(
  api: Api,
  native: boolean,
  notify: (message: string) => void,
) {
  type Owner = {
    api: Api;
    native: boolean;
    live: boolean;
    epoch: number;
    reading: boolean;
    refreshPending: boolean;
  };
  const ownership = useRef<Owner>({
    api,
    native,
    live: false,
    epoch: 0,
    reading: false,
    refreshPending: false,
  });
  if (ownership.current.api !== api || ownership.current.native !== native) {
    ownership.current.live = false;
    ownership.current = {
      api,
      native,
      live: false,
      epoch: 0,
      reading: false,
      refreshPending: false,
    };
  }
  const owner = ownership.current;
  const [snapshot, setSnapshot] = useState(() => ({
    owner,
    value: defaultLauncherPreferenceView(),
  }));
  const view =
    snapshot.owner === owner ? snapshot.value : defaultLauncherPreferenceView();
  const current = useRef(view);
  current.current = view;
  const [busyOwner, setBusyOwner] = useState<Owner | null>(null);
  const busy = busyOwner === owner;
  const working = useRef<{ owner: Owner; epoch: number } | null>(null);
  if (working.current && working.current.owner !== owner)
    working.current = null;
  const [revealed, setRevealed] = useState(false);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [importEpoch, setImportEpoch] = useState(0);
  const callbacks = useRef({ notify });
  callbacks.current = { notify };
  const owns = () => ownership.current === owner && owner.live && owner.native;
  function publish(value: LauncherPreferenceView) {
    current.current = value;
    setSnapshot({ owner, value });
    if (value.runtime_warning) callbacks.current.notify(value.runtime_warning);
  }
  function refresh() {
    if (!owns() || working.current || owner.reading) return;
    owner.refreshPending = false;
    owner.reading = true;
    const epoch = ++owner.epoch;
    void api<LauncherPreferenceView>("launcher_preferences")
      .then((value) => {
        if (owns() && owner.epoch === epoch && !working.current) publish(value);
      })
      .catch((error) => {
        if (owns() && owner.epoch === epoch)
          callbacks.current.notify(String(error));
      })
      .finally(() => {
        owner.reading = false;
        if (owner.refreshPending) refresh();
      });
  }
  useEffect(() => {
    if (!native) return;
    owner.live = true;
    owner.refreshPending = true;
    refresh();
    const unsubscribe = subscribeLauncherAppearanceApplied(() => {
      ++owner.epoch;
      owner.refreshPending = true;
      refresh();
    });
    return () => {
      unsubscribe();
      owner.live = false;
      ++owner.epoch;
    };
  }, [owner]);
  function adopt(value: LauncherPreferenceView) {
    if (owns() && (!working.current || working.current.epoch === owner.epoch))
      publish(value);
  }
  async function run<T>(work: () => Promise<T>): Promise<T> {
    if (!native || !current.current.revision)
      throw new Error(t("prefs.desktop"));
    if (working.current) throw new Error(t("prefs.busy"));
    if (!owns()) throw new Error(t("prefs.desktop"));
    owner.refreshPending ||= owner.reading;
    const operation = { owner, epoch: ++owner.epoch };
    working.current = operation;
    setBusyOwner(owner);
    try {
      return await work();
    } catch (error) {
      // An outside edit or an ambiguous disk failure may have blocked writes.
      // Snapshot exposes that warning; an explicit reload is a separate action.
      try {
        if (owns() && working.current === operation) {
          const value = await api<LauncherPreferenceView>(
            "launcher_preferences",
          );
          if (working.current === operation) adopt(value);
        }
      } catch {
        /* Preserve the last known revision when the IPC itself is unavailable. */
      }
      throw error;
    } finally {
      if (working.current === operation) {
        working.current = null;
        if (owns()) {
          setBusyOwner(null);
          if (owner.refreshPending) refresh();
        }
      }
    }
  }
  function patch(value: LauncherPreferencePatch) {
    return run(async () => {
      const next = await api<LauncherPreferenceView>(
        "launcher_preferences_update",
        { revision: current.current.revision, patch: value },
      );
      adopt(next);
      return next;
    });
  }
  async function action(action: LauncherAction) {
    await run(async () => {
      const revision = current.current.revision;
      if (action === "export_settings") {
        const result = await api<{ path: string } | null>(
          "launcher_export_settings",
          { revision },
        );
        if (result)
          callbacks.current.notify(t("prefs.exported", { path: result.path }));
      } else if (action === "import_settings") {
        const preview = await api<ImportPreview | null>(
          "launcher_prepare_settings_import",
          { revision },
        );
        if (preview) setConfirmation({ kind: "import", preview, error: "" });
      } else throw new Error(t("prefs.actionUnavailable"));
    });
  }
  async function confirm() {
    if (!confirmation || working.current) return;
    const submitted = confirmation;
    try {
      await run(async () => {
        const next = await api<LauncherPreferenceView>(
          "launcher_apply_settings_import",
          {
            token: submitted.preview.token,
            revision: submitted.preview.revision,
          },
        );
        adopt(next);
        setConfirmation(null);
        setImportEpoch((value) => value + 1);
        callbacks.current.notify(
          next.last_import_backup_path
            ? t("prefs.importedBackup", { path: next.last_import_backup_path })
            : t("prefs.imported"),
        );
      });
    } catch (error) {
      setConfirmation((value) =>
        value === submitted ? { ...value, error: String(error) } : value,
      );
    }
  }
  const prefs = view.preferences;
  function isHidden(id: LauncherMenuId) {
    return !revealed && prefs.navigation.hidden_menu_ids.includes(id);
  }
  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      if (event.key !== "F12") return;
      event.preventDefault();
      // Recovery is always available, including imported selections that hide
      // the settings page and the feature-hiding controls themselves.
      setRevealed((value) => !value);
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, []);
  useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      document.documentElement.dataset.launcherTheme =
        prefs.appearance.theme === "system"
          ? media.matches
            ? "dark"
            : "light"
          : prefs.appearance.theme;
    };
    apply();
    media.addEventListener("change", apply);
    document.documentElement.style.setProperty(
      "--ce-opacity",
      String(prefs.appearance.opacity_percent / 100),
    );
    return () => media.removeEventListener("change", apply);
  }, [prefs.appearance.theme, prefs.appearance.opacity_percent]);
  useEffect(() => {
    const root = document.documentElement;
    root.dataset.customCursor = String(prefs.appearance.custom_cursor);
    root.dataset.cursorStyle = prefs.appearance.cursor_style;
    root.dataset.cursorSize = prefs.appearance.cursor_size;
  }, [
    prefs.appearance.custom_cursor,
    prefs.appearance.cursor_style,
    prefs.appearance.cursor_size,
  ]);
  useEffect(
    () =>
      animateLauncher(prefs.animation.fps_limit, prefs.animation.speed_percent),
    [prefs.animation.fps_limit, prefs.animation.speed_percent],
  );
  const confirmationUi = confirmation ? (
    <InstanceOperationDialog
      title={t("prefs.import")}
      titleId="launcher-import-settings-title"
      busy={busy}
      committing={busy}
      confirmLabel={t("prefs.apply")}
      confirmDisabled={busy}
      onConfirm={() => void confirm()}
      onClose={() => {
        if (!working.current) setConfirmation(null);
      }}
    >
      <p className="launcher-setting-summary">{t("prefs.importHelp")}</p>
      <dl className="launcher-setting-summary">
        <dt>{t("prefs.appearance")}</dt>
        <dd>
          {confirmation.preview.preferences.appearance.theme} ·{" "}
          {formatNumber(
            confirmation.preview.preferences.appearance.opacity_percent,
          )}
          %
        </dd>
        <dt>{t("prefs.localization")}</dt>
        <dd>
          {confirmation.preview.preferences.localization.language} /{" "}
          {confirmation.preview.preferences.localization.region}
        </dd>
        <dt>{t("prefs.hidden")}</dt>
        <dd>
          {formatNumber(
            confirmation.preview.preferences.navigation.hidden_menu_ids.length,
          )}
        </dd>
        <dt>{t("prefs.home")}</dt>
        <dd>
          {confirmation.preview.preferences.home.mode}{" "}
          {confirmation.preview.preferences.home.local_path ||
            confirmation.preview.preferences.home.remote_url}
        </dd>
        <dt>{t("misc.proxy")}</dt>
        <dd>
          {confirmation.preview.preferences.network.proxy_mode}{" "}
          {confirmation.preview.preferences.network.custom_proxy_url}
        </dd>
      </dl>
      {confirmation.error && (
        <p className="rd-name-error" role="alert">
          {confirmation.error}
        </p>
      )}
    </InstanceOperationDialog>
  ) : null;
  return {
    view,
    prefs,
    busy,
    patch,
    action,
    adopt,
    run,
    revealed,
    isHidden,
    effects,
    confirmationUi,
    importEpoch,
  };
}
