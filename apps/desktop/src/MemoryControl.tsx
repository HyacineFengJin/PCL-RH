import { useEffect, useRef, useState } from "react";
import { useGameFieldSave } from "./GameIdentityFields";
import { formatNumber, t } from "./i18n";
import type { Api, MemoryPreview, Settings } from "./types";
import "./memory-control.css";

type MemoryChoice = "global" | "auto" | "custom";
type SystemInfo = {
  total_memory_bytes: number;
  available_memory_bytes: number;
};

function own<T>(map: Record<string, T> | undefined, id: string) {
  return map && Object.hasOwn(map, id) ? map[id] : undefined;
}

function memoryChoice(settings: Settings, id?: string): MemoryChoice {
  if (id === undefined)
    return settings.memory_mode === "auto" ? "auto" : "custom";
  const mode = own(settings.memory_mode_overrides, id);
  if (mode === "auto") return "auto";
  return mode === "manual" || own(settings.overrides, id) !== undefined
    ? "custom"
    : "global";
}

function withMemoryChoice(
  settings: Settings,
  id: string | undefined,
  choice: MemoryChoice,
  memory: number,
): Settings {
  if (id === undefined)
    return {
      ...settings,
      memory_mode: choice === "auto" ? "auto" : "manual",
      ...(choice === "custom" ? { memory_gib: memory } : {}),
    };
  const modes = { ...settings.memory_mode_overrides };
  const overrides = { ...settings.overrides };
  if (choice === "global") {
    delete modes[id];
    delete overrides[id];
    return { ...settings, memory_mode_overrides: modes, overrides };
  }
  return {
    ...settings,
    memory_mode_overrides: {
      ...modes,
      [id]: choice === "auto" ? "auto" : "manual",
    },
    overrides: choice === "custom" ? { ...overrides, [id]: memory } : overrides,
  };
}

/** A preview belongs to one saved root/revision/instance. Replacing this
 * identity during render also retires replies before effect cleanup runs. */
function useMemoryPreview({
  api,
  settings,
  instanceId,
  enabled,
}: {
  api: Api;
  settings: Settings;
  instanceId?: string;
  enabled: boolean;
}) {
  const scope = JSON.stringify([
    settings.root_id,
    settings.root,
    settings.revision,
    instanceId,
    enabled,
  ]);
  const identity = useRef({ api, scope });
  if (identity.current.api !== api || identity.current.scope !== scope)
    identity.current = { api, scope };
  const owner = identity.current;
  const live = useRef(true);
  const request = useRef<{ owner: typeof owner; token: symbol } | null>(null);
  const [state, setState] = useState<{
    owner: typeof owner;
    value: MemoryPreview | null;
    loading: boolean;
    error: string;
  } | null>(null);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      request.current = null;
    };
  }, []);
  const current = state?.owner === owner ? state : null;
  async function refresh() {
    if (
      !enabled ||
      !live.current ||
      identity.current !== owner ||
      request.current?.owner === owner
    )
      return;
    const token = Symbol();
    request.current = { owner, token };
    setState({ owner, value: null, loading: true, error: "" });
    const isCurrent = () =>
      live.current &&
      identity.current === owner &&
      request.current?.token === token;
    try {
      const value = await api<MemoryPreview>("memory_preview", {
        id: instanceId ?? null,
      });
      if (!isCurrent()) return;
      if (
        !settings.root_id ||
        !settings.revision ||
        value.root_id !== settings.root_id ||
        value.instance_id !== (instanceId ?? null) ||
        value.settings_revision !== settings.revision ||
        value.mode !== "auto"
      )
        throw new Error(t("memory.previewChanged"));
      if (value.error) throw new Error(value.error);
      if (
        !Number.isInteger(value.memory_gib) ||
        value.memory_gib < 2 ||
        value.memory_gib > 64 ||
        !Number.isSafeInteger(value.total_memory_bytes) ||
        !Number.isSafeInteger(value.available_memory_bytes) ||
        (value.total_memory_bytes ?? 0) <= 0 ||
        (value.available_memory_bytes ?? -1) < 0 ||
        value.available_memory_bytes! > value.total_memory_bytes!
      )
        throw new Error(t("memory.previewChanged"));
      setState({ owner, value, loading: false, error: "" });
    } catch (error) {
      if (isCurrent())
        setState({ owner, value: null, loading: false, error: String(error) });
    } finally {
      if (request.current?.token === token) request.current = null;
    }
  }
  useEffect(() => {
    void refresh();
  }, [owner]);
  return { ...current, refresh };
}

/** Both launch settings pages share the existing PCL memory card. Manual
 * drafts survive unrelated saves; radio selection always reflects saved data. */
export function MemoryControl({
  settings,
  instanceId,
  api,
  native,
  disabled,
  onSave,
  onNotify,
}: {
  settings: Settings;
  instanceId?: string;
  api: Api;
  native: boolean;
  disabled: boolean;
  onSave: (settings: Settings) => Promise<void>;
  onNotify: (message: string) => void;
}) {
  const instance = instanceId !== undefined;
  const choice = memoryChoice(settings, instanceId);
  const automatic =
    choice === "auto" ||
    (choice === "global" && settings.memory_mode === "auto");
  const manual = instance
    ? (own(settings.overrides, instanceId) ?? settings.memory_gib)
    : settings.memory_gib;
  const draftKey = JSON.stringify([
    settings.root_id,
    settings.root,
    instanceId,
  ]);
  const [draft, setDraft] = useState({
    key: draftKey,
    source: manual,
    value: manual,
  });
  const memory =
    draft.key === draftKey && draft.source === manual ? draft.value : manual;
  useEffect(() => {
    setDraft({ key: draftKey, source: manual, value: manual });
  }, [draftKey, manual]);
  const binding = useRef({ api, generation: 0 });
  if (binding.current.api !== api)
    binding.current = { api, generation: binding.current.generation + 1 };
  const action = useGameFieldSave({
    scope: JSON.stringify([
      draftKey,
      settings.revision,
      binding.current.generation,
    ]),
    disabled: !native || disabled,
    onNotify,
  });
  const preview = useMemoryPreview({
    api,
    settings,
    instanceId,
    enabled: native && !disabled && automatic,
  });
  const [system, setSystem] = useState<{ api: Api; value: SystemInfo } | null>(
    null,
  );
  useEffect(() => {
    let live = true;
    if (native)
      api<SystemInfo>("system_info")
        .then((value) => {
          if (live) setSystem({ api, value });
        })
        .catch(() => {});
    return () => {
      live = false;
    };
  }, [api, native]);
  // The allocation, warning and memory bar must describe the same snapshot.
  // A refreshed automatic estimate cannot keep the page's older RAM figures.
  const systemInfo = automatic
    ? preview.value
      ? {
          total_memory_bytes: preview.value.total_memory_bytes!,
          available_memory_bytes: preview.value.available_memory_bytes!,
        }
      : null
    : native && system?.api === api
      ? system.value
      : null;
  const allocation = automatic
    ? (preview.value?.memory_gib ?? null)
    : choice === "global"
      ? settings.memory_gib
      : memory;
  const used = systemInfo
    ? Math.max(
        0,
        systemInfo.total_memory_bytes - systemInfo.available_memory_bytes,
      )
    : 0;
  const gib = (value: number) =>
    formatNumber(value, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  function choose(next: MemoryChoice, nextMemory = memory) {
    if (
      !action.allowed() ||
      (next === choice && (next !== "custom" || nextMemory === manual))
    )
      return;
    void action.save(() =>
      onSave(withMemoryChoice(settings, instanceId, next, nextMemory)),
    );
  }
  const custom = choice === "custom";
  return (
    <section className="ce-card ce-memory-card">
      <h2 className="ce-card-title">{t("instance.memory")}</h2>
      {systemInfo &&
        allocation !== null &&
        allocation * 1073741824 > systemInfo.available_memory_bytes && (
          <div className="ce-memory-warning">{t("settings.memoryWarning")}</div>
        )}
      {instance && (
        <label className="ce-memory-mode">
          <input
            type="radio"
            name="instance-memory"
            value="global"
            checked={choice === "global"}
            disabled={action.unavailable}
            onChange={() => choose("global")}
          />
          {t("ui.followGlobal")}
        </label>
      )}
      <label className="ce-memory-mode">
        <input
          type="radio"
          name={instance ? "instance-memory" : "global-memory"}
          value="auto"
          checked={choice === "auto"}
          disabled={action.unavailable}
          onChange={() => choose("auto")}
        />
        {t("settings.auto")}
      </label>
      <div className="ce-memory-custom">
        <label className="ce-memory-mode">
          <input
            type="radio"
            name={instance ? "instance-memory" : "global-memory"}
            value="custom"
            checked={custom}
            disabled={action.unavailable}
            onChange={() => choose("custom")}
          />
          {t("settings.custom")}
        </label>
        <input
          type="range"
          aria-label={t(
            instance ? "instance.memoryLabel" : "settings.memoryGib",
          )}
          min="2"
          max={Math.min(
            64,
            Math.max(
              14,
              memory,
              Math.floor((systemInfo?.total_memory_bytes ?? 0) / 1073741824),
            ),
          )}
          step="1"
          value={memory}
          disabled={action.unavailable || !custom}
          onChange={(event) => {
            if (custom && action.allowed())
              setDraft({
                key: draftKey,
                source: manual,
                value: Number(event.target.value),
              });
          }}
          onPointerUp={() => {
            if (custom) choose("custom");
          }}
          onKeyUp={() => {
            if (custom) choose("custom");
          }}
          onBlur={() => {
            if (custom) choose("custom");
          }}
        />
      </div>
      {automatic && (
        <div className="ce-memory-preview">
          <span
            className="ce-game-field-hint"
            role={preview.error ? "alert" : "status"}
          >
            {preview.error
              ? t("memory.previewFailed", {
                  error: preview.error.replace(/\s+/g, " ").slice(0, 180),
                })
              : preview.loading
                ? t("memory.loading")
                : t("memory.recalculate")}
            {preview.value?.constrained && ` · ${t("memory.constrained")}`}
          </span>
          <button
            type="button"
            className="ce-button"
            disabled={
              !native || disabled || !!preview.loading || action.unavailable
            }
            onClick={() => void preview.refresh()}
          >
            {t("memory.refresh")}
          </button>
        </div>
      )}
      <div className="ce-memory-labels">
        <span>
          {t(instance ? "instance.usedMemory" : "settings.usedTotal")}
        </span>
        <span>{t("settings.allocated")}</span>
      </div>
      <div className="ce-memory-bar">
        <span
          style={{
            width:
              systemInfo && systemInfo.total_memory_bytes > 0
                ? `${Math.min(100, (used / systemInfo.total_memory_bytes) * 100)}%`
                : "0%",
          }}
        />
      </div>
      <div className="ce-memory-values">
        <span>
          {systemInfo
            ? `${gib(used / 1073741824)} GiB / ${gib(systemInfo.total_memory_bytes / 1073741824)} GiB`
            : t("settings.memoryUnavailable")}
        </span>
        <span>
          {allocation === null ? "—" : `${gib(allocation)} GiB`}
          {systemInfo
            ? t("settings.availableMemory", {
                memory: gib(systemInfo.available_memory_bytes / 1073741824),
              })
            : ""}
        </span>
      </div>
    </section>
  );
}
