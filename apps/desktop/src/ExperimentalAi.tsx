/** The form holds only a newly typed key. Saved credentials remain host-owned;
 * loading a preset returns routing/budget metadata and a key-present flag. */
import { useEffect, useRef, useState } from "react";
import { Play, Square } from "lucide-react";
import { CeSelect } from "./CeSelect";
import { t, serviceError, type MessageKey } from "./i18n";
import type { Api } from "./types";
import {
  experimentalCall as call,
  type EngineStatus,
  type PiCatalog,
  type AiPreset,
  type AiPresetStore,
  type PiWorkLimits,
} from "./experimentalTypes";
import "./experimental.css";
const customProvider = "pcl-custom";
const fallbackLimits: PiWorkLimits = {
  modelTurns: 32,
  toolCalls: 128,
  maximumOutputTokens: 8192,
  wallTimeMs: 900000,
  estimatedBudgetUsd: 5,
};
const limitFields: {
  key: keyof PiWorkLimits;
  label: MessageKey;
  scale?: number;
}[] = [
  { key: "modelTurns", label: "experimental.modelTurns" },
  { key: "toolCalls", label: "experimental.toolCalls" },
  { key: "maximumOutputTokens", label: "experimental.outputTokens" },
  { key: "wallTimeMs", label: "experimental.timeSeconds", scale: 1000 },
  { key: "estimatedBudgetUsd", label: "experimental.budgetUsd" },
];
export function ExperimentalAi({ api, native }: { api: Api; native: boolean }) {
  const [catalog, setCatalog] = useState<PiCatalog | null>(null);
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [store, setStore] = useState<AiPresetStore | null>(null);
  const [presetId, setPresetId] = useState<string | null>(null),
    [name, setName] = useState("");
  const [provider, setProvider] = useState("openai"),
    [model, setModel] = useState("");
  const [protocol, setProtocol] = useState("openai-completions"),
    [baseUrl, setBaseUrl] = useState("");
  const [contextWindow, setContextWindow] = useState("32768"),
    [thinking, setThinking] = useState("off");
  const [inputPrice, setInputPrice] = useState(""),
    [outputPrice, setOutputPrice] = useState("");
  const [limits, setLimits] = useState<PiWorkLimits>(fallbackLimits);
  const [key, setKey] = useState(""),
    [clearKey, setClearKey] = useState(false),
    [error, setError] = useState("");
  const [busy, setBusy] = useState(false),
    [loaded, setLoaded] = useState(false);
  const alive = useRef(false),
    working = useRef(false),
    epoch = useRef(0);
  const custom = provider === customProvider;
  const entry = catalog?.providers.find((p) => p.id === provider);
  const selectedModel = entry?.models.find((m) => m.id === model);
  const saved = store?.presets.find((p) => p.id === presetId);
  const routeMatches =
    !!saved &&
    saved.selection.provider === provider &&
    (!custom ||
      (saved.selection.baseUrl === baseUrl.trim().replace(/\/+$/, "") &&
        saved.selection.api === protocol));
  const retainKey = routeMatches && saved.selection.keyConfigured && !clearKey;
  const disabled = !native || busy || !loaded || !!store?.warning;
  function chooseProvider(id: string, source = catalog) {
    setProvider(id);
    setKey("");
    setClearKey(false);
    const models = source?.providers.find((p) => p.id === id)?.models ?? [];
    setModel(
      id === customProvider
        ? ""
        : ((models.find((m) => /mini|flash/i.test(m.id)) ?? models[0])?.id ??
            ""),
    );
  }
  function applyPreset(
    preset: AiPreset | undefined,
    source = catalog,
    data = store,
  ) {
    setKey("");
    setClearKey(false);
    setPresetId(preset?.id ?? null);
    setName(preset?.name ?? t("experimental.newPreset"));
    setLimits(preset?.limits ?? data?.defaults ?? fallbackLimits);
    if (!preset) {
      chooseProvider("openai", source);
      setThinking("off");
      setBaseUrl("");
      setProtocol("openai-completions");
      setContextWindow("32768");
      setInputPrice("");
      setOutputPrice("");
      return;
    }
    const value = preset.selection;
    setProvider(value.provider);
    setModel(value.model);
    setProtocol(value.api);
    setBaseUrl(value.baseUrl);
    setContextWindow(String(value.contextWindow));
    setThinking(value.thinking);
    setInputPrice(value.cost ? String(value.cost.input) : "");
    setOutputPrice(value.cost ? String(value.cost.output) : "");
  }
  useEffect(() => {
    alive.current = true;
    let valid = true,
      polling = false;
    if (native)
      void Promise.all([
        call<PiCatalog>(api, "provider_catalog"),
        call<EngineStatus>(api, "status"),
      ])
        .then(([source, state]) => {
          if (!valid) return;
          setCatalog(source);
          setStatus(state);
          setStore(state.aiPresets);
          applyPreset(
            state.aiPresets.presets.find(
              (p) => p.id === state.aiPresets.selectedId,
            ),
            source,
            state.aiPresets,
          );
          setLoaded(true);
        })
        .catch((e) => {
          if (valid) setError(serviceError(e));
        });
    const timer = setInterval(() => {
      if (!native || !valid || polling || working.current) return;
      polling = true;
      const ticket = epoch.current;
      void call<EngineStatus>(api, "status")
        .then((state) => {
          if (valid && ticket === epoch.current) {
            setStatus(state);
            setStore(state.aiPresets);
          }
        })
        .catch((e) => {
          if (valid) setError(serviceError(e));
        })
        .finally(() => {
          polling = false;
        });
    }, 2500);
    return () => {
      valid = false;
      alive.current = false;
      clearInterval(timer);
    };
  }, [api, native]);
  async function perform(action: () => Promise<void>) {
    if (working.current) return;
    working.current = true;
    epoch.current++;
    setBusy(true);
    setError("");
    try {
      await action();
      const state = await call<EngineStatus>(api, "status");
      if (alive.current) {
        setStatus(state);
        setStore(state.aiPresets);
      }
    } catch (e) {
      if (alive.current) setError(serviceError(e));
    } finally {
      working.current = false;
      if (alive.current) setBusy(false);
    }
  }
  async function save() {
    const config = {
      provider,
      model: model.trim(),
      thinking: custom || !selectedModel?.reasoning ? "off" : thinking,
      // Blank input retains the stored key only for the same provider/endpoint.
      ...(retainKey && !key ? {} : { key }),
      ...(custom
        ? {
            api: protocol,
            baseUrl: baseUrl.trim(),
            contextWindow: Number(contextWindow),
            cost:
              inputPrice === "" && outputPrice === ""
                ? null
                : { input: Number(inputPrice), output: Number(outputPrice) },
          }
        : {}),
    };
    const data = await call<AiPresetStore>(api, "provider_preset_save", {
      id: presetId,
      name,
      config,
      limits,
    });
    if (alive.current) {
      setStore(data);
      applyPreset(
        data.presets.find((p) => p.id === data.selectedId),
        catalog,
        data,
      );
    }
  }
  async function choosePreset(id: string) {
    const data = await call<AiPresetStore>(api, "provider_preset_select", {
      id,
    });
    if (alive.current) {
      setStore(data);
      applyPreset(
        data.presets.find((p) => p.id === data.selectedId),
        catalog,
        data,
      );
    }
  }
  const pricesValid =
    !custom ||
    (inputPrice === "" && outputPrice === "") ||
    (inputPrice !== "" &&
      outputPrice !== "" &&
      [inputPrice, outputPrice].every(
        (v) =>
          Number.isFinite(Number(v)) && Number(v) >= 0 && Number(v) <= 10000,
      ));
  const limitsValid = Object.entries(limits).every(([key, value]) => {
    const [min, max] = store?.ranges[key as keyof PiWorkLimits] ?? [
      1,
      Infinity,
    ];
    return (
      Number.isFinite(value) &&
      (value === 0 || value >= min) &&
      value <= max &&
      (key === "estimatedBudgetUsd" || Number.isInteger(value))
    );
  });
  const formValid =
    !!name.trim() &&
    !!model.trim() &&
    (custom || !!key || retainKey) &&
    pricesValid &&
    limitsValid &&
    (!custom ||
      (!!baseUrl.trim() &&
        Number.isInteger(Number(contextWindow)) &&
        Number(contextWindow) >= 4096 &&
        Number(contextWindow) <= 2000000));
  return (
    <div className="experimental-panel experimental-ai">
      <section className="ce-card">
        <h2 className="ce-card-title">{t("experimental.ai")}</h2>
        <p>{t("experimental.aiHelp")}</p>
        {!native && (
          <p className="experimental-origin">{t("common.unavailable")}</p>
        )}
        <label className="ce-row">
          <span>{t("experimental.preset")}</span>
          <div className="experimental-preset-controls">
            <CeSelect
              className="ce-field"
              value={presetId ?? ""}
              disabled={disabled}
              onChange={(e) => void perform(() => choosePreset(e.target.value))}
            >
              <option value="" disabled>
                {t("experimental.newPreset")}
              </option>
              {(store?.presets ?? []).map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </CeSelect>
            <button
              className="ce-button"
              disabled={disabled || (store?.presets.length ?? 0) >= 32}
              onClick={() => applyPreset(undefined)}
            >
              {t("experimental.new")}
            </button>
            <button
              className="ce-button"
              disabled={disabled || !presetId}
              onClick={() =>
                void perform(async () => {
                  const data = await call<AiPresetStore>(
                    api,
                    "provider_preset_delete",
                    { id: presetId },
                  );
                  if (alive.current) {
                    setStore(data);
                    applyPreset(
                      data.presets.find((p) => p.id === data.selectedId),
                      catalog,
                      data,
                    );
                  }
                })
              }
            >
              {t("experimental.deletePreset")}
            </button>
          </div>
        </label>
        <label className="ce-row">
          <span>{t("experimental.presetName")}</span>
          <input
            className="ce-field"
            value={name}
            maxLength={80}
            disabled={disabled}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <label className="ce-row">
          <span>{t("experimental.provider")}</span>
          <CeSelect
            className="ce-field"
            value={provider}
            disabled={disabled}
            onChange={(e) => chooseProvider(e.target.value)}
          >
            {(catalog?.providers ?? []).map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
            <option value={customProvider}>
              {t("experimental.customProvider")}
            </option>
          </CeSelect>
        </label>
        <label className="ce-row">
          <span>{t("experimental.model")}</span>
          {custom ? (
            <input
              className="ce-field"
              value={model}
              disabled={disabled}
              maxLength={256}
              onChange={(e) => setModel(e.target.value)}
            />
          ) : (
            <CeSelect
              className="ce-field"
              value={model}
              disabled={disabled}
              onChange={(e) => setModel(e.target.value)}
            >
              {(entry?.models ?? []).map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name} · {m.id}
                </option>
              ))}
            </CeSelect>
          )}
        </label>
        {custom && (
          <>
            <label className="ce-row">
              <span>{t("experimental.protocol")}</span>
              <CeSelect
                className="ce-field"
                value={protocol}
                disabled={disabled}
                onChange={(e) => setProtocol(e.target.value)}
              >
                <option value="openai-completions">
                  OpenAI Chat Completions
                </option>
                <option value="openai-responses">OpenAI Responses</option>
                <option value="anthropic-messages">Anthropic Messages</option>
              </CeSelect>
            </label>
            <label className="ce-row">
              <span>{t("experimental.baseUrl")}</span>
              <input
                className="ce-field"
                type="url"
                value={baseUrl}
                disabled={disabled}
                maxLength={2048}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
            </label>
            <p className="experimental-origin">
              {t("experimental.baseUrlHelp")}
            </p>
          </>
        )}
        <label className="ce-row">
          <span>{t("experimental.thinking")}</span>
          <CeSelect
            className="ce-field"
            value={custom || !selectedModel?.reasoning ? "off" : thinking}
            disabled={disabled || custom || !selectedModel?.reasoning}
            onChange={(e) => setThinking(e.target.value)}
          >
            <option value="off">{t("experimental.thinkingOff")}</option>
            <option value="minimal">Minimal</option>
            <option value="low">Low</option>
            <option value="medium">Medium</option>
            <option value="high">High</option>
          </CeSelect>
        </label>
        {!custom && selectedModel && (
          <p className="experimental-origin">
            {selectedModel.api} · {selectedModel.baseUrl}
          </p>
        )}
        <label className="ce-row">
          <span>{t("experimental.key")}</span>
          <input
            className="ce-field"
            type="password"
            autoComplete="new-password"
            spellCheck={false}
            value={key}
            placeholder={retainKey ? t("experimental.keyRetained") : ""}
            disabled={disabled}
            maxLength={2048}
            onChange={(e) => {
              setKey(e.target.value);
              setClearKey(false);
            }}
          />
        </label>
        {custom && (
          <p className="experimental-origin">
            {t("experimental.customKeyHelp")}
          </p>
        )}
        {custom && routeMatches && saved.selection.keyConfigured && (
          <label className="ce-check">
            <input
              type="checkbox"
              checked={clearKey}
              disabled={disabled}
              onChange={(e) => {
                setClearKey(e.target.checked);
                if (e.target.checked) setKey("");
              }}
            />
            <span>{t("experimental.clearSavedKey")}</span>
          </label>
        )}
        <details className="experimental-advanced">
          <summary>{t("experimental.advanced")}</summary>
          <p className="experimental-origin">{t("experimental.limitHelp")}</p>
          {limitFields.map(({ key, label, scale = 1 }) => (
            <label className="ce-row" key={key}>
              <span>{t(label)}</span>
              <input
                className="ce-field"
                type="number"
                min={0}
                max={store ? store.ranges[key][1] / scale : undefined}
                step={key === "estimatedBudgetUsd" ? "any" : 1}
                value={Number.isFinite(limits[key]) ? limits[key] / scale : ""}
                disabled={disabled}
                onChange={(e) =>
                  setLimits((v) => ({
                    ...v,
                    // Empty is invalid; only an explicitly entered 0 removes a limit.
                    [key]: e.target.valueAsNumber * scale,
                  }))
                }
              />
            </label>
          ))}
          {saved && !saved.selection.costKnown && (
            <p className="experimental-origin">
              {t("experimental.unknownPrice")}
            </p>
          )}
          <div className="ce-actions">
            <button
              className="ce-button"
              disabled={disabled}
              onClick={() => setLimits(store?.defaults ?? fallbackLimits)}
            >
              {t("experimental.resetLimits")}
            </button>
          </div>
          {custom && (
            <>
              <label className="ce-row">
                <span>{t("experimental.contextWindow")}</span>
                <input
                  className="ce-field"
                  type="number"
                  min={4096}
                  max={2000000}
                  step={1}
                  value={contextWindow}
                  disabled={disabled}
                  onChange={(e) => setContextWindow(e.target.value)}
                />
              </label>
              <p className="experimental-origin">
                {t("experimental.customPrices")}
              </p>
              <label className="ce-row">
                <span>{t("experimental.inputPrice")}</span>
                <input
                  className="ce-field"
                  type="number"
                  min={0}
                  max={10000}
                  step="any"
                  value={inputPrice}
                  disabled={disabled}
                  onChange={(e) => setInputPrice(e.target.value)}
                />
              </label>
              <label className="ce-row">
                <span>{t("experimental.outputPrice")}</span>
                <input
                  className="ce-field"
                  type="number"
                  min={0}
                  max={10000}
                  step="any"
                  value={outputPrice}
                  disabled={disabled}
                  onChange={(e) => setOutputPrice(e.target.value)}
                />
              </label>
            </>
          )}
        </details>
        <div className="ce-actions">
          <button
            className="ce-button"
            disabled={disabled || !formValid}
            onClick={() => void perform(save)}
          >
            {t("experimental.savePreset")}
          </button>
        </div>
        <div className="experimental-launch-controls">
          <button
            className="launch-button"
            disabled={disabled || !formValid}
            onClick={() =>
              void perform(async () => {
                await save();
                await call(api, "provider_start");
              })
            }
          >
            <span>
              <Play size={18} />
              {t("experimental.start")}
            </span>
            <small>
              {status?.liveConfigured
                ? t("experimental.running")
                : t("experimental.autoStart")}
            </small>
          </button>
          <button
            className="launch-button"
            disabled={disabled || !status?.liveConfigured}
            onClick={() =>
              void perform(async () => {
                await call(api, "provider_stop");
              })
            }
          >
            <span>
              <Square size={15} />
              {t("experimental.stop")}
            </span>
            <small>{t("experimental.stopHelp")}</small>
          </button>
        </div>
        {status?.liveSelection && (
          <p className="experimental-provider-status" role="status">
            {t("experimental.running")}: {status.liveSelection.model}
          </p>
        )}
        {store?.warning && (
          <p className="experimental-error" role="alert">
            {store.warning}
          </p>
        )}
        {error && (
          <p className="experimental-error" role="alert">
            {error}
          </p>
        )}
      </section>
    </div>
  );
}
