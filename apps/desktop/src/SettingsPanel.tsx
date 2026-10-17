import { CeSelect } from "./CeSelect";
import { t, formatNumber } from "./i18n";
import { useEffect, useState } from "react";
import { ArrowRight, ChevronDown } from "lucide-react";
import "./settings-panel.css";
import { Collapse } from "./Collapse";
import { JavaPanel } from "./JavaPanel";
import { GameTitleField, IsolationField } from "./GameIdentityFields";
import { GameLaunchFields } from "./GameLaunchFields";
import type {
  LauncherPreferenceView,
  LauncherPreferencePatch,
} from "./launcherTypes";
import type { Api, Settings } from "./types";
type SystemInfo = {
  total_memory_bytes: number;
  available_memory_bytes: number;
};
type Props = {
  section: string;
  settings: Settings;
  onSave: (settings: Settings) => Promise<void>;
  api: Api;
  native: boolean;
  disabled: boolean;
  onInstances: () => void;
  showInstanceSettings?: boolean;
  launcherPreferences?: LauncherPreferenceView;
  onLauncherPatch?: (
    patch: LauncherPreferencePatch,
  ) => Promise<LauncherPreferenceView>;
  launcherBusy?: boolean;
  exitAfterLaunchAvailable?: boolean;
  onNotify: (message: string) => void;
  onRefresh: () => Promise<void>;
  downloadDisabled?: boolean;
  onJavaTask?: (id: string, reveal: boolean) => void;
  javaRecommendation?: { name: string; major: number };
  onInstanceJava?: () => void;
};
const unavailable = () => t("settings.unavailable");
const jvm =
  "-XX:+UseG1GC -XX:-UseAdaptiveSizePolicy -XX:-OmitStackTraceInFastThrow -Djdk.lang.Process.allowAmbiguousCommands=true -Dfml.ignoreInvalidMinecraftCertificates=True -Dfml.ignorePatchDiscrepancies=True -Dlog4j2.formatMsgNoLookups=true";
function Field({
  label,
  value,
  multiline = false,
  dropdown = false,
}: {
  label: string;
  value: string;
  multiline?: boolean;
  dropdown?: boolean;
}) {
  return (
    <label className="ce-settings-row">
      <span>{label}</span>
      {multiline ? (
        <textarea
          className="ce-field"
          value={value}
          readOnly
          disabled
          title={unavailable()}
        />
      ) : (
        <div className="ce-settings-control">
          <input
            className="ce-field"
            value={value}
            readOnly
            disabled
            title={unavailable()}
          />
          {dropdown && <ChevronDown size={17} />}
        </div>
      )}
    </label>
  );
}
export function SettingsPanel({
  section,
  settings,
  onSave,
  api,
  native,
  disabled,
  onInstances,
  showInstanceSettings = true,
  launcherPreferences,
  onLauncherPatch,
  launcherBusy = false,
  exitAfterLaunchAvailable = false,
  onNotify,
  onRefresh,
  downloadDisabled,
  onJavaTask,
  javaRecommendation,
  onInstanceJava,
}: Props) {
  const [memory, setMemory] = useState(settings.memory_gib);
  const [advanced, setAdvanced] = useState(false);
  const [system, setSystem] = useState<SystemInfo | null>(null);
  useEffect(() => {
    setMemory(settings.memory_gib);
  }, [settings.memory_gib]);
  useEffect(() => {
    let current = true;
    api<SystemInfo>("system_info")
      .then((v) => {
        if (current) setSystem(v);
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [api, native]);
  const saveMemory = async () => {
    if (memory === settings.memory_gib || disabled) return;
    try {
      await onSave({ ...settings, memory_gib: memory });
    } catch (e) {
      onNotify(String(e));
    }
  };
  if (section === "java")
    return (
      <JavaPanel
        settings={settings}
        api={api}
        native={native}
        disabled={disabled}
        onSave={onSave}
        onRefresh={onRefresh}
        onNotify={onNotify}
        downloadDisabled={downloadDisabled}
        onJavaTask={onJavaTask}
        recommendation={javaRecommendation}
        onInstanceJava={onInstanceJava}
      />
    );
  if (!["launch", "启动", "game"].includes(section))
    return (
      <section className="ce-card">
        <h2 className="ce-card-title">{t("settings.pageUnavailable")}</h2>
      </section>
    );
  const gib = (bytes: number) =>
    formatNumber(bytes / 1073741824, {
      minimumFractionDigits: 1,
      maximumFractionDigits: 1,
    });
  const used = system
    ? system.total_memory_bytes - system.available_memory_bytes
    : 0;
  return (
    <div className="ce-settings-panel">
      <section className="ce-card">
        <h2 className="ce-card-title">{t("instance.launchOptions")}</h2>
        <div className="ce-settings-fields">
          <IsolationField
            settings={settings}
            onSave={onSave}
            scope={`${settings.root_id}:${settings.root}:${settings.revision}:${section}`}
            disabled={!native || disabled || !!downloadDisabled}
            onNotify={onNotify}
          />
          <GameTitleField
            value={
              launcherPreferences?.preferences.game_launch?.window_title || ""
            }
            scope={`${settings.root_id}:${settings.root}:${launcherPreferences?.revision}:${section}`}
            disabled={
              !native ||
              disabled ||
              launcherBusy ||
              !onLauncherPatch ||
              !launcherPreferences?.revision ||
              !!launcherPreferences.warning
            }
            onNotify={onNotify}
            onSave={async (title) => {
              await onLauncherPatch?.({
                game_launch: { window_title: title || "" },
              });
            }}
          />
          <Field label={t("settings.customInfo")} value="PCL CE" />
          <label className="ce-settings-row">
            <span>{t("settings.visibility")}</span>
            <CeSelect
              className="ce-field"
              value={
                launcherPreferences?.preferences.launch_visibility || "always"
              }
              disabled={
                !native ||
                !onLauncherPatch ||
                !launcherPreferences?.revision ||
                !!launcherPreferences.warning ||
                launcherBusy
              }
              onChange={(event) =>
                void onLauncherPatch?.({
                  launch_visibility: event.target
                    .value as LauncherPreferenceView["preferences"]["launch_visibility"],
                }).catch((error) => onNotify(String(error)))
              }
            >
              <option value="always">{t("settings.alwaysVisible")}</option>
              <option value="hide_while_game">
                {t("settings.hideWhileGame")}
              </option>
              <option
                value="exit_after_launch"
                disabled={!exitAfterLaunchAvailable}
              >
                {t("settings.exitAfterLaunch")}
              </option>
            </CeSelect>
          </label>
          <Field
            dropdown
            label={t("settings.priority")}
            value={t("settings.balanced")}
          />
          <GameLaunchFields
            field="window"
            view={launcherPreferences}
            native={native}
            disabled={disabled}
            busy={launcherBusy}
            onPatch={onLauncherPatch}
            onNotify={onNotify}
          />
          <Field
            dropdown
            label={t("settings.authentication")}
            value={t("settings.deviceFlow")}
          />
          <GameLaunchFields
            field="ip"
            view={launcherPreferences}
            native={native}
            disabled={disabled}
            busy={launcherBusy}
            onPatch={onLauncherPatch}
            onNotify={onNotify}
          />
        </div>
      </section>
      <section className="ce-card ce-memory-card">
        <h2 className="ce-card-title">{t("instance.memory")}</h2>
        {system && memory * 1073741824 > system.available_memory_bytes && (
          <div className="ce-memory-warning">{t("settings.memoryWarning")}</div>
        )}
        <label className="ce-memory-mode" title={unavailable()}>
          <input type="radio" disabled checked={false} readOnly />
          {t("settings.auto")}
        </label>
        <div className="ce-memory-custom">
          <label className="ce-memory-mode">
            <input type="radio" checked readOnly />
            {t("settings.custom")}
          </label>
          <input
            aria-label={t("settings.memoryGib")}
            type="range"
            min="2"
            max={Math.max(14, settings.memory_gib)}
            step="1"
            value={memory}
            disabled={disabled}
            onChange={(e) => setMemory(Number(e.target.value))}
            onPointerUp={() => void saveMemory()}
            onKeyUp={() => void saveMemory()}
            onBlur={() => void saveMemory()}
          />
        </div>
        <div className="ce-memory-labels">
          <span>{t("settings.usedTotal")}</span>
          <span>{t("settings.allocated")}</span>
        </div>
        <div className="ce-memory-bar">
          <span
            style={{
              width: system
                ? `${Math.min(100, (used / system.total_memory_bytes) * 100)}%`
                : "0%",
            }}
          />
        </div>
        <div className="ce-memory-values">
          <span>
            {system
              ? `${gib(used)} GiB / ${gib(system.total_memory_bytes)} GiB`
              : t("settings.memoryUnavailable")}
          </span>
          <span>
            {formatNumber(memory, {
              minimumFractionDigits: 1,
              maximumFractionDigits: 1,
            })}{" "}
            GiB
            {system
              ? t("settings.availableMemory", {
                  memory: gib(system.available_memory_bytes),
                })
              : ""}
          </span>
        </div>
      </section>
      <section className="ce-card ce-advanced-card">
        <button
          className="ce-advanced-heading"
          onClick={() => setAdvanced(!advanced)}
          aria-expanded={advanced}
        >
          <h2 className="ce-card-title">{t("settings.advanced")}</h2>
          <ChevronDown
            className={`ce-disclosure-arrow ${advanced ? "is-open" : ""}`}
            size={20}
          />
        </button>
        <Collapse open={advanced}>
          <div className="ce-advanced-content">
            <Field
              dropdown
              label={t("settings.renderer")}
              value={t("settings.gameDefault")}
            />
            <Field label={t("settings.jvm")} value={jvm} multiline />
            <Field label={t("settings.gameArgs")} value="" />
            <Field label={t("settings.command")} value="" />
            <div className="ce-advanced-checks">
              {[
                t("settings.disableWrapper"),
                t("settings.disableLegacyFix"),
                t("settings.gpu"),
                t("settings.javaExe"),
                t("settings.disableUnsafe"),
                t("settings.disableCrash"),
                t("settings.lockMemory"),
              ].map((label, i) => (
                <label key={label} title={unavailable()}>
                  <input
                    type="checkbox"
                    checked={i === 0 || i === 2}
                    disabled
                    readOnly
                  />
                  {label}
                </label>
              ))}
            </div>
          </div>
        </Collapse>
      </section>
      <Collapse open={advanced && showInstanceSettings}>
        <div className="ce-settings-footer">
          <button className="ce-button" onClick={onInstances}>
            <ArrowRight size={20} />
            {t("settings.instance")}
          </button>
        </div>
      </Collapse>
    </div>
  );
}
