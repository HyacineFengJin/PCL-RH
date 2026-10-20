import { t, formatNumber, type MessageKey } from "./i18n";
import { useEffect, useState } from "react";
import {
  ArrowLeft,
  Box,
  ChevronDown,
  Download,
  Gauge,
  LoaderCircle,
  X,
} from "lucide-react";
import type { Api } from "./types";
import { Collapse } from "./Collapse";
import { loaderCandidates } from "./loaderCandidates";
import grass from "./assets/game-icons/grass.png";
import forge from "./assets/game-icons/forge.png";
import neoForge from "./assets/game-icons/neoforge.png";
import fabric from "./assets/game-icons/fabric.png";
import "./install-selection.css";
const providers = ["Forge", "NeoForge", "Fabric", "LabyMod", "OptiFine"];
const unavailableProviders: Record<string, MessageKey> = {
  LabyMod: "install.labyUnavailable",
  OptiFine: "install.optifineUnavailable",
};

export type InstallComponent = { provider: string; version: string };
export type InstallOptions = {
  name: string;
  components: InstallComponent[];
};

export function installNameError(name: string, installed: { id: string }[]) {
  if (!name.trim()) return t("instance.nameRequired");
  if (name !== name.trim()) return t("instance.nameWhitespace");
  if (name.startsWith(".install-")) return t("instance.namePrefix");
  if (
    name === "." ||
    name === ".." ||
    /[/\\:]/.test(name) ||
    /\p{Cc}/u.test(name)
  )
    return t("instance.nameCharacters");
  if (new TextEncoder().encode(name).length > 120)
    return t("instance.nameLong");
  if (installed.some((instance) => instance.id === name))
    return t("instance.nameExists");
  return "";
}

function ComponentIcon({ name }: { name: string }) {
  if (name === "Forge" || name === "NeoForge" || name === "Fabric")
    return (
      <img
        className={name === "Forge" ? "game-icon-anvil" : undefined}
        src={name === "Forge" ? forge : name === "NeoForge" ? neoForge : fabric}
        alt=""
      />
    );
  const Icon = name === "OptiFine" ? Gauge : Box;
  return <Icon size={29} strokeWidth={1.6} />;
}
export function InstallSelection({
  api,
  rootId,
  version,
  native,
  disabled,
  installed = [],
  onBack,
  onStart,
}: {
  api: Api;
  rootId: string | null;
  version: string;
  native: boolean;
  disabled: boolean;
  installed?: { id: string }[];
  onBack: () => void;
  onStart: (options: InstallOptions) => void;
}) {
  const [name, setName] = useState(version);
  const [nameEdited, setNameEdited] = useState(false);
  const [open, setOpen] = useState<string[]>([]);
  const [choices, setChoices] = useState<Record<string, string>>({});
  const [catalogs, setCatalogs] = useState<Record<string, string[]>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState<string[]>([]);
  useEffect(() => {
    let disposed = false;
    setName(version);
    setNameEdited(false);
    setOpen([]);
    setChoices({});
    setCatalogs({});
    setErrors({});
    setLoading(providers);
    for (const provider of providers) {
      const request = loaderCandidates(api, provider, version);
      request
        .then((values) => {
          if (!disposed) setCatalogs((old) => ({ ...old, [provider]: values }));
        })
        .catch((e) => {
          if (!disposed)
            setErrors((old) => ({ ...old, [provider]: String(e) }));
        })
        .finally(() => {
          if (!disposed) setLoading((old) => old.filter((v) => v !== provider));
        });
    }
    return () => {
      disposed = true;
    };
  }, [api, version, rootId]);
  function conflict(provider: string) {
    const selected = Object.keys(choices);
    if (provider === "OptiFine")
      return selected.some((v) =>
        ["NeoForge", "Fabric", "LabyMod"].includes(v),
      );
    return (
      selected.some((v) => v !== provider && v !== "OptiFine") ||
      (provider !== "Forge" && !!choices.OptiFine)
    );
  }
  const components = providers.flatMap((provider) =>
    choices[provider] ? [{ provider, version: choices[provider] }] : [],
  );
  const automaticName = [
    version,
    ...components.map(
      (component) => `${component.provider}_${component.version}`,
    ),
  ].join("-");
  const instanceName = nameEdited ? name : automaticName;
  const nameError = installNameError(instanceName, installed);
  const componentError = components
    .map((component) =>
      unavailableProviders[component.provider]
        ? t(unavailableProviders[component.provider])
        : undefined,
    )
    .find(Boolean);
  const canInstall = native && !disabled && !nameError && !componentError;
  const unavailable = !native
    ? t("install.previewUnavailable")
    : nameError ||
      componentError ||
      (disabled ? t("install.startUnavailable") : "");
  function submit() {
    if (canInstall) onStart({ name: instanceName, components });
  }
  return (
    <div className="ce-install-selection ce-page-enter">
      <section className="ce-card ce-install-name">
        <button
          className="icon-button"
          aria-label={t("install.back")}
          onClick={onBack}
        >
          <ArrowLeft size={19} />
        </button>
        <img src={grass} alt="" />
        <input
          className="ce-field"
          aria-label={t("instance.name")}
          value={instanceName}
          maxLength={120}
          disabled={disabled}
          aria-invalid={!!nameError}
          aria-describedby={nameError ? "ce-install-name-error" : undefined}
          onChange={(e) => {
            setNameEdited(true);
            setName(e.target.value);
          }}
        />
      </section>
      {nameError && (
        <p
          className="ce-install-name-error"
          id="ce-install-name-error"
          role="status"
        >
          {nameError}
        </p>
      )}
      {providers.map((provider) => {
        const values = catalogs[provider] || [];
        const incompatible = conflict(provider);
        const unavailableProvider = unavailableProviders[provider]
          ? t(unavailableProviders[provider])
          : undefined;
        const expanded = open.includes(provider);
        const subtitle =
          choices[provider] ||
          unavailableProvider ||
          (incompatible
            ? t("install.incompatible")
            : loading.includes(provider)
              ? t("ui.loading")
              : errors[provider]
                ? t("ui.temporarilyUnavailable")
                : values.length
                  ? t("install.addable")
                  : t("install.noVersion"));
        return (
          <section
            className={`ce-card ce-install-provider ${incompatible ? "incompatible" : ""}`}
            key={provider}
          >
            <button
              className="ce-install-toggle"
              onClick={() =>
                setOpen((old) =>
                  old.includes(provider)
                    ? old.filter((v) => v !== provider)
                    : [...old, provider],
                )
              }
              aria-expanded={expanded}
            >
              <strong>{provider}</strong>
              <span>{expanded && !choices[provider] ? "" : subtitle}</span>
              <ChevronDown
                size={18}
                className={`ce-disclosure-arrow ${expanded ? "is-open" : ""}`}
              />
            </button>
            <Collapse open={expanded}>
              <div className="ce-install-versions">
                {unavailableProvider && (
                  <p className="muted" role="status">
                    {unavailableProvider}
                  </p>
                )}
                {choices[provider] && (
                  <button
                    className="ce-button"
                    disabled={disabled}
                    onClick={() =>
                      setChoices((old) => {
                        const next = { ...old };
                        delete next[provider];
                        return next;
                      })
                    }
                  >
                    <X size={13} />
                    {t("ui.deselect")}
                  </button>
                )}
                {loading.includes(provider) ? (
                  <p className="muted">
                    <LoaderCircle size={16} className="spin" />{" "}
                    {t("install.loadingCompatible")}
                  </p>
                ) : errors[provider] ? (
                  <p className="muted" role="status">
                    {errors[provider]}
                  </p>
                ) : !values.length ? (
                  <p className="muted">
                    {t("install.noMinecraftVersion", { version })}
                  </p>
                ) : (
                  <>
                    <button
                      className={`ce-install-version ${choices[provider] === values[0] ? "selected" : ""}`}
                      disabled={
                        disabled || incompatible || !!unavailableProvider
                      }
                      onClick={() =>
                        setChoices((old) => ({ ...old, [provider]: values[0] }))
                      }
                    >
                      <ComponentIcon name={provider} />
                      <span>
                        <strong>{values[0]}</strong>
                        <small>{t("ui.latest")}</small>
                      </span>
                    </button>
                    <div className="ce-install-all">
                      {t("install.allVersions", {
                        count: formatNumber(values.length),
                      })}
                    </div>
                    {values.map((value) => (
                      <button
                        key={value}
                        className={`ce-install-version ${choices[provider] === value ? "selected" : ""}`}
                        disabled={
                          disabled || incompatible || !!unavailableProvider
                        }
                        onClick={() =>
                          setChoices((old) => ({ ...old, [provider]: value }))
                        }
                      >
                        <ComponentIcon name={provider} />
                        <span>
                          <strong>{value}</strong>
                        </span>
                      </button>
                    ))}
                  </>
                )}
              </div>
            </Collapse>
          </section>
        );
      })}
      <div className="ce-install-action">
        <button
          className="ce-pill-action"
          disabled={!canInstall}
          title={unavailable || undefined}
          onClick={submit}
        >
          <Download size={19} />
          {t("install.start")}
        </button>
        {unavailable && <small role="status">{unavailable}</small>}
      </div>
    </div>
  );
}
