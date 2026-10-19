import { CeSelect } from "./CeSelect";
import { t } from "./i18n";
import { useState } from "react";
import { ArrowRight, ChevronDown } from "lucide-react";
import "./settings-panel.css";
import { Collapse } from "./Collapse";
import { JavaPanel } from "./JavaPanel";
import { GameTitleField, IsolationField } from "./GameIdentityFields";
import { GameLaunchFields } from "./GameLaunchFields";
import { MemoryControl } from "./MemoryControl";
import {
  GameArgumentField,
  type GameArgumentField as ArgumentKey,
} from "./GameArgumentFields";
import type {
  LauncherPreferenceView,
  LauncherPreferencePatch,
} from "./launcherTypes";
import type { Api, Settings } from "./types";
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
function Field({
  label,
  value,
  reason = unavailable(),
  dropdown = false,
}: {
  label: string;
  value: string;
  reason?: string;
  dropdown?: boolean;
}) {
  return (
    <label className="ce-settings-row">
      <span>{label}</span>
      <div className="ce-settings-control">
        <input
          className="ce-field"
          value={value}
          readOnly
          disabled
          title={reason}
        />
        {dropdown && <ChevronDown size={17} />}
      </div>
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
  const [advanced, setAdvanced] = useState(false);
  function argumentField(field: ArgumentKey) {
    return (
      <GameArgumentField
        field={field}
        value={launcherPreferences?.preferences.game_launch?.[field] ?? ""}
        draftKey={`${settings.root_id}:${settings.root}:${section}:${field}`}
        scope={`${settings.root_id}:${settings.root}:${launcherPreferences?.revision}:${section}:${field}`}
        busy={launcherBusy}
        disabled={
          !native ||
          disabled ||
          !onLauncherPatch ||
          !launcherPreferences?.revision ||
          !!launcherPreferences.warning
        }
        onNotify={onNotify}
        onSave={async (value) => {
          await onLauncherPatch?.({ game_launch: { [field]: value ?? "" } });
        }}
      />
    );
  }
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
            busy={launcherBusy}
            disabled={
              !native ||
              disabled ||
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
          {argumentField("custom_info")}
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
            label={t("settings.authentication")}
            value={t("settings.deviceFlow")}
            reason={t("settings.deviceFlowOnly")}
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
      <MemoryControl
        settings={settings}
        api={api}
        native={native}
        disabled={disabled || !!downloadDisabled}
        onSave={onSave}
        onNotify={onNotify}
      />
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
            {argumentField("jvm_args")}
            {argumentField("game_args")}
            <p className="ce-game-field-hint">{t("settings.argumentsHint")}</p>
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
              ].map((label) => (
                <label key={label} title={unavailable()}>
                  <input type="checkbox" checked={false} disabled readOnly />
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
