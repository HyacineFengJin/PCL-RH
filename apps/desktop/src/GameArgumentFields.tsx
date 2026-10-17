import { useEffect, useState } from "react";
import { CeSelect } from "./CeSelect";
import { useGameFieldSave } from "./GameIdentityFields";
import { t } from "./i18n";
import type { LaunchArgumentOverrides, Settings } from "./types";

export type GameArgumentField = keyof LaunchArgumentOverrides;
const labels = {
  custom_info: "settings.customInfo",
  jvm_args: "settings.jvm",
  game_args: "settings.gameArgs",
} as const;

/** Each field saves explicitly. Empty overrides intentionally suppress the
 * global value; only removing the override restores inheritance. Validation
 * of argument syntax belongs to the same Rust parser used by launch planning. */
export function GameArgumentField({
  field,
  value,
  inherited = false,
  draftKey,
  onSave,
  ...props
}: {
  field: GameArgumentField;
  value: string | null | undefined;
  inherited?: boolean;
  draftKey: string;
  scope: string;
  disabled: boolean;
  busy?: boolean;
  onNotify: (message: string) => void;
  onSave: (value: string | undefined) => Promise<void>;
}) {
  const { allowed, save, unavailable } = useGameFieldSave(props);
  const [draft, setDraft] = useState(value ?? "");
  const [independent, setIndependent] = useState(false);
  useEffect(() => {
    setDraft(value ?? "");
    setIndependent(false);
  }, [field, value, draftKey]);
  const followsGlobal = inherited && value == null && !independent;
  const multiline = field !== "custom_info";
  const label = t(labels[field]);
  function submit() {
    if (!allowed() || followsGlobal) return;
    void save(() => onSave(draft));
  }
  const editor = {
    className: "ce-field",
    "aria-label": label,
    value: draft,
    disabled: unavailable,
    placeholder: t(
      multiline ? "settings.noExtraArguments" : "settings.versionInfoDefault",
    ),
    spellCheck: false,
    onChange: (
      event: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>,
    ) => {
      if (allowed()) setDraft(event.target.value);
    },
    onKeyDown: (event: React.KeyboardEvent) => {
      if (
        event.key === "Enter" &&
        (!multiline || event.ctrlKey || event.metaKey)
      ) {
        event.preventDefault();
        submit();
      }
    },
  };
  return (
    <div className="ce-game-argument-field">
      {inherited && (
        <label className="ce-settings-row">
          <span>{label}</span>
          <CeSelect
            className="ce-field"
            aria-label={t("settings.argumentScope", { name: label })}
            value={followsGlobal ? "inherit" : "custom"}
            disabled={unavailable}
            onChange={(event) => {
              if (!allowed()) return;
              if (event.target.value === "custom") setIndependent(true);
              else if (event.target.value === "inherit") {
                if (value == null) {
                  setIndependent(false);
                  setDraft("");
                } else void save(() => onSave(undefined));
              }
            }}
          >
            <option value="inherit">{t("ui.followGlobal")}</option>
            <option value="custom">{t("settings.independentArguments")}</option>
          </CeSelect>
        </label>
      )}
      {!followsGlobal && (
        <div className="ce-settings-row ce-game-argument-row">
          <span>{inherited ? "" : label}</span>
          <div className="ce-game-argument-editor">
            {multiline ? (
              <textarea {...editor} rows={3} />
            ) : (
              <input {...editor} />
            )}
            <button
              type="button"
              className="ce-button"
              disabled={
                unavailable || draft === (inherited ? value : (value ?? ""))
              }
              aria-label={t("settings.saveArgument", { name: label })}
              onClick={submit}
            >
              {t("settings.saveArguments")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

export function withInstanceArgument(
  settings: Settings,
  instanceId: string,
  field: GameArgumentField,
  value: string | undefined,
): Settings {
  const overrides = { ...settings.argument_overrides };
  const entry = { ...overrides[instanceId] };
  if (value === undefined) delete entry[field];
  else entry[field] = value;
  if (Object.values(entry).some((value) => value != null))
    overrides[instanceId] = entry;
  else delete overrides[instanceId];
  return { ...settings, argument_overrides: overrides };
}
