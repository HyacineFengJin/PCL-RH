import { useEffect, useRef, useState } from "react";
import { CeSelect } from "./CeSelect";
import { t } from "./i18n";
import type { IsolationPolicy, Settings } from "./types";

type SavedField = {
  scope: string;
  disabled: boolean;
  onNotify: (message: string) => void;
};

/** Saved handlers may survive a root/settings or disabled/enabled round trip.
 * A fresh identity retires them immediately, before effect cleanup. Busy state
 * still belongs to the admitted save until it settles, so changing views cannot
 * admit a second write while the first callback is publishing settings. */
function useFieldSave({ scope, disabled, onNotify }: SavedField) {
  const identity = useRef({ scope, disabled });
  if (
    identity.current.scope !== scope ||
    identity.current.disabled !== disabled
  )
    identity.current = { scope, disabled };
  const owner = identity.current;
  const live = useRef(true),
    working = useRef(false);
  const current = useRef({ disabled, onNotify });
  current.current = { disabled, onNotify };
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const allowed = () =>
    live.current &&
    identity.current === owner &&
    !current.current.disabled &&
    !working.current;
  const save = async (action: () => Promise<void>) => {
    if (!allowed()) return;
    working.current = true;
    setSaving(true);
    try {
      await action();
    } catch (error) {
      if (live.current && identity.current === owner)
        current.current.onNotify(String(error));
    } finally {
      working.current = false;
      if (live.current) setSaving(false);
    }
  };
  return { allowed, save, unavailable: disabled || saving };
}

export function IsolationField({
  settings,
  instanceId,
  onSave,
  ...props
}: SavedField & {
  settings: Settings;
  instanceId?: string;
  onSave: (settings: Settings) => Promise<void>;
}) {
  const { save, unavailable } = useFieldSave(props);
  const value = instanceId
    ? settings.isolation_overrides?.[instanceId] || "inherit"
    : settings.isolation || "automatic";
  function choose(value: string) {
    if (!["inherit", "automatic", "shared", "isolated"].includes(value)) return;
    if (!instanceId && value === "inherit") return;
    const next = { ...settings };
    if (instanceId) {
      const overrides = { ...settings.isolation_overrides };
      if (value === "inherit") delete overrides[instanceId];
      else overrides[instanceId] = value as IsolationPolicy;
      next.isolation_overrides = overrides;
    } else next.isolation = value as IsolationPolicy;
    void save(() => onSave(next));
  }
  return (
    <>
      <label className="ce-settings-row">
        <span>
          {t(instanceId ? "instance.isolation" : "settings.isolation")}
        </span>
        <CeSelect
          className="ce-field"
          value={value}
          disabled={unavailable}
          onChange={(event) => choose(event.target.value)}
        >
          {instanceId && (
            <option value="inherit">{t("ui.followGlobal")}</option>
          )}
          <option value="automatic">{t("settings.autoIsolation")}</option>
          <option value="isolated">{t("settings.isolationOn")}</option>
          <option value="shared">{t("settings.isolationOff")}</option>
        </CeSelect>
      </label>
      <p className="ce-game-field-hint">{t("settings.isolationHint")}</p>
    </>
  );
}

export function GameTitleField({
  value,
  inherited = false,
  onSave,
  ...props
}: SavedField & {
  value: string | undefined;
  inherited?: boolean;
  onSave: (title: string | undefined) => Promise<void>;
}) {
  const { allowed, save, unavailable } = useFieldSave(props);
  const [custom, setCustom] = useState(false);
  const [draft, setDraft] = useState(value || "");
  useEffect(() => {
    setDraft(value || "");
    setCustom(false);
  }, [value, props.scope]);
  const mode = custom
    ? "custom"
    : inherited && value === undefined
      ? "inherit"
      : value
        ? "custom"
        : "default";
  function choose(mode: string) {
    if (!allowed()) return;
    if (mode === "custom") {
      setCustom(true);
      return;
    }
    if (mode !== "default" && !(mode === "inherit" && inherited)) return;
    setCustom(false);
    void save(() => onSave(mode === "inherit" ? undefined : ""));
  }
  function saveTitle() {
    if (!allowed()) return;
    if (
      !draft.trim() ||
      [...draft].length > 256 ||
      /[\u0000-\u001f\u007f-\u009f]/u.test(draft)
    ) {
      props.onNotify(t("settings.titleInvalid"));
      return;
    }
    void save(() => onSave(draft));
  }
  return (
    <>
      <label className="ce-settings-row">
        <span>{t("settings.windowTitle")}</span>
        <CeSelect
          className="ce-field"
          value={mode}
          disabled={unavailable}
          onChange={(event) => choose(event.target.value)}
        >
          {inherited && <option value="inherit">{t("ui.followGlobal")}</option>}
          <option value="default">{t("instance.defaultTitle")}</option>
          <option value="custom">{t("ui.custom")}</option>
        </CeSelect>
      </label>
      {mode === "custom" && (
        <div className="ce-settings-row ce-game-title-editor">
          <span />
          <div className="ce-game-title-input">
            <input
              className="ce-field"
              aria-label={t("settings.windowTitle")}
              value={draft}
              disabled={unavailable}
              placeholder="PCL RH"
              onChange={(event) => {
                if (allowed()) setDraft(event.target.value);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  saveTitle();
                }
              }}
            />
            <button
              type="button"
              className="ce-button"
              disabled={unavailable || !draft.trim() || draft === value}
              onClick={saveTitle}
            >
              {t("settings.saveTitle")}
            </button>
          </div>
        </div>
      )}
    </>
  );
}
