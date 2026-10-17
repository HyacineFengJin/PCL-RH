import { GameTitleField, IsolationField } from "./GameIdentityFields";
import {
  GameArgumentField,
  withInstanceArgument,
  type GameArgumentField as ArgumentKey,
} from "./GameArgumentFields";
import { CeSelect } from "./CeSelect";
import { t, formatNumber, type MessageKey } from "./i18n";
import { useEffect, useRef, useState } from "react";
import {
  Box,
  ChevronDown,
  Search,
  Flame,
  Layers,
  Globe,
  Image,
  Server,
  Puzzle,
  Sparkles,
  Blocks,
  Play,
  Settings as SettingsIcon,
  Signal,
  FolderOpen,
} from "lucide-react";
import { Collapse } from "./Collapse";
import { InstanceOperations } from "./InstanceOperations";
import { JavaSelect } from "./JavaSelect";
import { InstanceDelete } from "./InstanceTrash";
import { ResourcePanel, type LocalResourceDetails } from "./LocalResources";
export type { LocalResourceDetails } from "./LocalResources";
import commandIcon from "./assets/game-icons/command.png";
import lampTexture from "./assets/game-icons/redstone-lamp.png";
import grassIcon from "./assets/game-icons/grass.png";
import neoForgeIcon from "./assets/game-icons/neoforge.png";
import forgeIcon from "./assets/game-icons/forge.png";
import steveIcon from "./assets/game-icons/steve.png";
import type {
  Api,
  Instance,
  InstanceMetadata,
  MetaView,
  Settings,
} from "./types";

export const instancePages: {
  id: string;
  label: string;
  labelKey: MessageKey;
  group?: string;
  groupKey?: MessageKey;
  icon: React.ElementType | null;
  unavailable?: boolean;
}[] = [
  {
    id: "overview",
    label: "概览",
    labelKey: "instance.overview",
    group: "游戏本体",
    groupKey: "instance.game",
    icon: Blocks,
  },
  { id: "settings", label: "设置", labelKey: "nav.settings", icon: null },
  { id: "modify", label: "修改", labelKey: "nav.modify", icon: null },
  { id: "export", label: "导出", labelKey: "nav.export", icon: Box },
  {
    id: "saves",
    label: "存档",
    labelKey: "nav.saves",
    group: "游戏资源",
    groupKey: "instance.resources",
    icon: Globe,
  },
  {
    id: "screenshots",
    label: "截图",
    labelKey: "nav.screenshots",
    icon: Image,
  },
  { id: "mods", label: "模组", labelKey: "resources.mods", icon: Puzzle },
  {
    id: "resourcepacks",
    label: "资源包",
    labelKey: "nav.resourcepacks",
    icon: Layers,
  },
  {
    id: "shaderpacks",
    label: "光影包",
    labelKey: "nav.shaderpacks",
    icon: Sparkles,
  },
  {
    id: "litematics",
    label: "投影原理图",
    labelKey: "nav.schematics",
    icon: Blocks,
  },
  {
    id: "server",
    label: "服务器",
    labelKey: "nav.server",
    icon: Server,
    unavailable: true,
  },
];

const defaultMetadata: InstanceMetadata = {
  description: "",
  favorite: false,
  icon: "auto",
  category: "auto",
};
const categoryGroups: Record<
  Exclude<InstanceMetadata["category"], "auto">,
  string
> = {
  vanilla: "Vanilla",
  forge: "Forge",
  neoforge: "NeoForge",
  fabric: "Fabric",
  quilt: "Quilt",
};
const builtInIcons = {
  grass: grassIcon,
  forge: forgeIcon,
  neoforge: neoForgeIcon,
  command: commandIcon,
  steve: steveIcon,
};
type MetadataScope = {
  api: Api;
  id: string;
  section: string;
  root: string;
};
type RenamePlan = {
  revision: string;
  id: string;
  new_name: string;
  dependent_instances: string[];
};
type RenameDialog = {
  scope: MetadataScope;
  draft: string;
  plan: RenamePlan | null;
  error: string;
};
export function instanceRenameNameError(
  name: string,
  current: string,
  occupiedNames: string[],
): string {
  if (!name.trim()) return t("instance.nameRequired");
  if (name !== name.trim()) return t("instance.nameWhitespace");
  if (
    name === "." ||
    name === ".." ||
    /[\\/:\u0000-\u001f\u007f-\u009f]/.test(name)
  )
    return t("instance.nameCharacters");
  if (name.startsWith(".install-")) return t("instance.namePrefix");
  if (new TextEncoder().encode(name).length > 120)
    return t("instance.nameLong");
  if (name === current) return t("instance.nameDifferent");
  if (occupiedNames.includes(name)) return t("instance.nameExists");
  return "";
}
export function InstanceIcon({
  loader = "Vanilla",
  icon = "auto",
}: {
  loader?: string;
  icon?: InstanceMetadata["icon"];
}) {
  const automaticIcon =
    loader === "Vanilla"
      ? grassIcon
      : loader.startsWith("NeoForge")
        ? neoForgeIcon
        : loader.startsWith("Forge")
          ? forgeIcon
          : null;
  const image = icon === "auto" ? automaticIcon : builtInIcons[icon];
  return (
    <span className="instance-icon" aria-hidden="true">
      {image ? <img src={image} alt="" /> : <Box size={29} strokeWidth={1.4} />}
    </span>
  );
}
function Tags({ instance }: { instance: Instance }) {
  return (
    <div className="instance-tags">
      <span>{instance.minecraft_version}</span>
      {instance.loader !== "Vanilla" && <span>{instance.loader}</span>}
    </div>
  );
}
export function InstanceSelection({
  instances,
  query,
  setQuery,
  onPick,
  disabled,
}: {
  instances: Instance[];
  query: string;
  setQuery: (q: string) => void;
  onPick: (id: string) => void;
  disabled: boolean;
}) {
  const groupFor = (instance: Instance) => {
    const category = instance.metadata?.category || "auto";
    return category === "auto"
      ? instance.loader.split(" ")[0]
      : categoryGroups[category];
  };
  const search = query.toLowerCase();
  const matches = (instance: Instance) =>
    instance.id.toLowerCase().includes(search) ||
    (instance.metadata?.description || "").toLowerCase().includes(search);
  const visible = instances.filter(matches);
  const groups = [
    ...(visible.some((v) => v.metadata?.favorite) ? ["favorites"] : []),
    ...new Set(visible.filter((v) => !v.metadata?.favorite).map(groupFor)),
  ];
  const [collapsed, setCollapsed] = useState<string[]>([]);
  return (
    <>
      <label className="ce-card ce-searchbar">
        <Search size={17} />
        <input
          autoFocus
          placeholder={t("instance.search")}
          aria-label={t("instance.search")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </label>
      {groups.map((group) => {
        const entries = visible.filter((v) =>
          group === "favorites"
            ? v.metadata?.favorite
            : !v.metadata?.favorite && groupFor(v) === group,
        );
        if (!entries.length) return null;
        const isOpen = !collapsed.includes(group);
        return (
          <section className="ce-card instance-group" key={group}>
            <button
              className="ce-collapse"
              onClick={() =>
                setCollapsed((old) =>
                  isOpen ? [...old, group] : old.filter((v) => v !== group),
                )
              }
              aria-expanded={isOpen}
            >
              <strong>
                {group === "favorites"
                  ? t("resources.favorites")
                  : t("instance.group", {
                      group: group === "Vanilla" ? t("ui.vanilla") : group,
                    })}{" "}
                ({formatNumber(entries.length)})
              </strong>
              <ChevronDown
                size={17}
                className={`ce-disclosure-arrow ${isOpen ? "is-open" : ""}`}
              />
            </button>
            <Collapse open={isOpen}>
              <div className="instance-group-list">
                {entries.map((v) => (
                  <button
                    className="resource-row"
                    key={v.id}
                    disabled={disabled}
                    onClick={() => onPick(v.id)}
                  >
                    <InstanceIcon loader={v.loader} icon={v.metadata?.icon} />
                    <div className="ce-resource-text">
                      <strong className="ce-instance-title">
                        <span>{v.id}</span>
                        {v.metadata?.description && (
                          <small
                            className="ce-instance-description"
                            title={v.metadata.description}
                          >
                            {v.metadata.description}
                          </small>
                        )}
                      </strong>
                      <Tags instance={v} />
                    </div>
                  </button>
                ))}
              </div>
            </Collapse>
          </section>
        );
      })}
      {!visible.length && (
        <section className="ce-card ce-empty">{t("instance.empty")}</section>
      )}
    </>
  );
}
export function InstancePanel({
  instance,
  section,
  settings,
  api,
  onSave,
  onOpen,
  onInspect,
  onLogs,
  logsDisabled,
  onTestLaunch,
  onNotify,
  onResourceDetails,
  onMetadataChange,
  disabled,
  mutationDisabled,
  native,
  onTaskStart,
  occupiedNames = [],
  resourceRefreshKey = "",
}: {
  instance: Instance;
  section: string;
  settings: Settings;
  api: Api;
  onSave: (s: Settings) => Promise<void>;
  onOpen: (kind: string) => void;
  onInspect: () => void;
  onLogs: () => void;
  logsDisabled: boolean;
  onTestLaunch: () => void;
  onNotify: (s: string) => void;
  onResourceDetails?: (resource: LocalResourceDetails) => void;
  onMetadataChange?: (id: string, meta: MetaView) => void;
  disabled: boolean;
  mutationDisabled?: boolean;
  native: boolean;
  onTaskStart: (id: string) => void;
  occupiedNames?: string[];
  resourceRefreshKey?: string;
}) {
  const rootScope = settings.root_id || settings.root;
  const metadataScope = useRef<MetadataScope>({
    api,
    id: instance.id,
    section,
    root: rootScope,
  });
  if (
    metadataScope.current.api !== api ||
    metadataScope.current.id !== instance.id ||
    metadataScope.current.section !== section ||
    metadataScope.current.root !== rootScope
  ) {
    metadataScope.current = { api, id: instance.id, section, root: rootScope };
  }
  const currentMetadataScope = metadataScope.current;
  const metadataLive = useRef(true);
  const metadataWorkingRef = useRef<symbol | null>(null);
  const [metadataActivity, setMetadataActivity] = useState<
    "save" | "read" | null
  >(null);
  const metadataWorking = metadataActivity !== null;
  const [savedMetadata, setSavedMetadata] = useState<{
    api: Api;
    id: string;
    root: string;
    sourceRevision?: string;
    meta: MetaView;
  } | null>(null);
  const [metadataFailure, setMetadataFailure] = useState<{
    scope: MetadataScope;
    message: string;
  } | null>(null);
  const [descriptionDialog, setDescriptionDialog] = useState<{
    scope: MetadataScope;
    draft: string;
    revision: string;
  } | null>(null);
  const descriptionButton = useRef<HTMLButtonElement>(null);
  const restoreDescriptionFocus = useRef<MetadataScope | null>(null);
  const [renameDialog, setRenameDialogState] = useState<RenameDialog | null>(
    null,
  );
  const renameDialogRef = useRef<RenameDialog | null>(null);
  const renameButton = useRef<HTMLButtonElement>(null);
  const renameOperation = useRef<{
    token: symbol;
    scope: MetadataScope;
    kind: "prepare" | "start";
  } | null>(null);
  const [renameActivity, setRenameActivity] = useState<{
    token: symbol;
    scope: MetadataScope;
    kind: "prepare" | "start";
  } | null>(null);
  const renameOpen = renameDialog?.scope === currentMetadataScope;
  const renameWorking = renameActivity?.scope === currentMetadataScope;
  const renameDisabled =
    !native || disabled || !!mutationDisabled || metadataWorking;
  const renameAvailability = useRef(renameDisabled);
  renameAvailability.current = renameDisabled;
  const renameNameError =
    renameOpen && renameDialog
      ? instanceRenameNameError(renameDialog.draft, instance.id, occupiedNames)
      : "";
  function setRenameDialog(next: RenameDialog | null) {
    renameDialogRef.current = next;
    setRenameDialogState(next);
  }
  function closeRename() {
    if (
      !metadataLive.current ||
      metadataScope.current !== currentMetadataScope ||
      renameDialogRef.current?.scope !== currentMetadataScope
    )
      return;
    if (
      renameOperation.current?.scope === currentMetadataScope &&
      renameOperation.current.kind === "start"
    )
      return;
    if (renameOperation.current?.scope === currentMetadataScope) {
      renameOperation.current = null;
      setRenameActivity(null);
    }
    setRenameDialog(null);
    if (metadataScope.current === currentMetadataScope)
      renameButton.current?.focus();
  }
  async function submitRename() {
    const dialog = renameDialogRef.current;
    if (
      !dialog ||
      currentMetadataScope !== metadataScope.current ||
      dialog.scope !== currentMetadataScope ||
      dialog.scope !== metadataScope.current ||
      !metadataLive.current ||
      renameAvailability.current ||
      instanceRenameNameError(dialog.draft, dialog.scope.id, occupiedNames) ||
      renameOperation.current?.scope === dialog.scope
    )
      return;
    const operation = {
      token: Symbol(),
      scope: dialog.scope,
      kind: dialog.plan ? ("start" as const) : ("prepare" as const),
    };
    renameOperation.current = operation;
    setRenameActivity(operation);
    const isCurrent = () =>
      metadataLive.current &&
      metadataScope.current === dialog.scope &&
      renameDialogRef.current === dialog;
    try {
      if (dialog.plan) {
        const { revision, id, new_name } = dialog.plan;
        const result = await api<{ id: string }>("instance_rename_start", {
          plan: { revision, id, new_name },
        });
        if (!isCurrent()) return;
        if (!result.id) throw new Error(t("instance.renameTaskMissing"));
        setRenameDialog(null);
        onTaskStart(result.id);
      } else {
        const plan = await api<RenamePlan>("instance_rename_prepare", {
          id: dialog.scope.id,
          newName: dialog.draft,
        });
        if (!isCurrent()) return;
        if (
          plan.id !== dialog.scope.id ||
          plan.new_name !== dialog.draft ||
          !plan.revision
        )
          throw new Error(t("instance.renamePlanChanged"));
        setRenameDialog({ ...dialog, plan, error: "" });
      }
    } catch (error) {
      if (isCurrent())
        setRenameDialog({ ...dialog, plan: null, error: String(error) });
    } finally {
      if (renameOperation.current?.token === operation.token) {
        renameOperation.current = null;
        if (metadataLive.current)
          setRenameActivity((old) =>
            old?.token === operation.token ? null : old,
          );
      }
    }
  }
  const hasSavedMetadata =
    savedMetadata?.api === api &&
    savedMetadata.id === instance.id &&
    savedMetadata.root === rootScope &&
    (savedMetadata.sourceRevision === instance.metadata_revision ||
      savedMetadata.meta.revision === instance.metadata_revision);
  const metadata = hasSavedMetadata
    ? savedMetadata.meta
    : instance.metadata || defaultMetadata;
  const metadataRevision = hasSavedMetadata
    ? savedMetadata.meta.revision
    : instance.metadata_revision;
  const metadataError =
    metadataFailure?.scope === currentMetadataScope
      ? metadataFailure.message
      : "";
  const descriptionOpen = descriptionDialog?.scope === currentMetadataScope;
  const metadataReadOnly = !metadataRevision;
  const metadataWritesDisabled =
    disabled ||
    !!mutationDisabled ||
    metadataReadOnly ||
    metadataWorking ||
    renameOpen ||
    renameWorking;
  const metadataReadOnlyMessage = t("instance.metadataReadonly");
  useEffect(() => {
    metadataLive.current = true;
    return () => {
      metadataLive.current = false;
    };
  }, []);
  useEffect(() => {
    if (
      !descriptionOpen &&
      !metadataWorking &&
      restoreDescriptionFocus.current
    ) {
      if (restoreDescriptionFocus.current === currentMetadataScope) {
        descriptionButton.current?.focus();
      }
      restoreDescriptionFocus.current = null;
    }
  }, [descriptionOpen, metadataWorking, currentMetadataScope]);
  function closeDescription() {
    if (metadataWorkingRef.current) return;
    setDescriptionDialog(null);
    descriptionButton.current?.focus();
  }
  function acceptMetadata(meta: MetaView) {
    setSavedMetadata({
      api,
      id: instance.id,
      root: rootScope,
      sourceRevision: instance.metadata_revision,
      meta,
    });
    onMetadataChange?.(instance.id, meta);
  }
  async function reloadMetadata() {
    if (
      metadataWorkingRef.current ||
      currentMetadataScope !== metadataScope.current
    )
      return;
    const operation = Symbol();
    metadataWorkingRef.current = operation;
    setMetadataActivity("read");
    const isCurrent = () =>
      metadataLive.current && metadataScope.current === currentMetadataScope;
    try {
      const meta = await api<MetaView>("instance_metadata_read", {
        id: instance.id,
      });
      if (!isCurrent()) return;
      acceptMetadata(meta);
      setMetadataFailure(null);
      if (descriptionOpen)
        restoreDescriptionFocus.current = currentMetadataScope;
      setDescriptionDialog(null);
      onNotify(t("instance.metadataReloaded"));
    } catch (error) {
      if (isCurrent()) {
        setMetadataFailure({
          scope: currentMetadataScope,
          message: String(error),
        });
      }
    } finally {
      if (metadataWorkingRef.current === operation) {
        metadataWorkingRef.current = null;
        if (metadataLive.current) setMetadataActivity(null);
      }
    }
  }
  async function updateMetadata(
    patch: Partial<InstanceMetadata>,
    successMessage: string,
    expectedRevision = metadataRevision,
  ) {
    if (
      metadataWritesDisabled ||
      metadataWorkingRef.current ||
      currentMetadataScope !== metadataScope.current ||
      !expectedRevision
    )
      return;
    const operation = Symbol();
    metadataWorkingRef.current = operation;
    setMetadataActivity("save");
    setMetadataFailure(null);
    const isCurrent = () =>
      metadataLive.current && metadataScope.current === currentMetadataScope;
    try {
      const meta = await api<MetaView>("instance_metadata_update", {
        id: instance.id,
        revision: expectedRevision,
        patch,
      });
      if (!isCurrent()) return;
      acceptMetadata(meta);
      if (patch.description !== undefined) {
        restoreDescriptionFocus.current = currentMetadataScope;
        setDescriptionDialog(null);
      }
      onNotify(successMessage);
    } catch (error) {
      if (isCurrent()) {
        setMetadataFailure({
          scope: currentMetadataScope,
          message: String(error),
        });
      }
    } finally {
      if (metadataWorkingRef.current === operation) {
        metadataWorkingRef.current = null;
        if (metadataLive.current) setMetadataActivity(null);
      }
    }
  }
  const [memory, setMemory] = useState(
    settings.overrides[instance.id] || settings.memory_gib,
  );
  const [mode, setMode] = useState(
    settings.overrides[instance.id] ? "custom" : "global",
  );
  const [system, setSystem] = useState<{
    total_memory_bytes: number;
    available_memory_bytes: number;
  } | null>(null);
  useEffect(() => {
    setMemory(settings.overrides[instance.id] || settings.memory_gib);
    setMode(settings.overrides[instance.id] ? "custom" : "global");
  }, [instance.id, settings]);
  useEffect(() => {
    let live = true;
    api<{ total_memory_bytes: number; available_memory_bytes: number }>(
      "system_info",
    )
      .then((v) => {
        if (live) setSystem(v);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [api]);
  async function saveMemory(nextMode = mode, nextMemory = memory) {
    const overrides = { ...settings.overrides };
    if (nextMode === "global") delete overrides[instance.id];
    else overrides[instance.id] = nextMemory;
    try {
      await onSave({ ...settings, overrides });
    } catch (e) {
      onNotify(String(e));
    }
  }
  function argumentField(field: ArgumentKey) {
    return (
      <GameArgumentField
        field={field}
        value={settings.argument_overrides?.[instance.id]?.[field]}
        inherited
        draftKey={`${rootScope}:${settings.root}:${instance.id}:${section}:${field}`}
        scope={`${rootScope}:${settings.root}:${settings.revision}:${instance.id}:${section}:${field}`}
        disabled={!native || disabled || !!mutationDisabled}
        onNotify={onNotify}
        onSave={(value) =>
          onSave(withInstanceArgument(settings, instance.id, field, value))
        }
      />
    );
  }
  if (section === "overview")
    return (
      <>
        <section className="ce-card instance-summary">
          <InstanceIcon loader={instance.loader} icon={metadata.icon} />
          <div className="ce-instance-summary-text">
            <div className="ce-instance-title">
              <span>{instance.id}</span>
              {metadata.description && (
                <small
                  className="ce-instance-description"
                  title={metadata.description}
                >
                  {metadata.description}
                </small>
              )}
            </div>
            <Tags instance={instance} />
          </div>
        </section>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("instance.information")}</h2>
          <div className="instance-info">
            <div>
              <span className="info-block">
                <svg viewBox="0 0 32 32" aria-hidden="true">
                  <image
                    href={lampTexture}
                    width="16"
                    height="16"
                    transform="matrix(1,-.5,1,.5,0,8)"
                  />
                  <image
                    href={lampTexture}
                    width="16"
                    height="16"
                    transform="matrix(1,.5,0,1,0,8)"
                  />
                  <image
                    href={lampTexture}
                    width="16"
                    height="16"
                    transform="matrix(1,-.5,0,1,16,16)"
                    style={{ filter: "brightness(.8)" }}
                  />
                </svg>
              </span>
              <div>
                {t("instance.launchCount")}
                <small>—</small>
              </div>
            </div>
            <div>
              <span className="info-block command">
                <img src={commandIcon} alt="" />
              </span>
              <div>
                {t("instance.packVersion")}
                <small>—</small>
              </div>
            </div>
            <div>
              <InstanceIcon />
              <div>
                Minecraft<small>{instance.minecraft_version}</small>
              </div>
            </div>
            {instance.loader !== "Vanilla" && (
              <div>
                <InstanceIcon loader={instance.loader} />
                <div>
                  {instance.loader.split(" ")[0]}
                  <small>{instance.loader.split(" ").slice(1).join(" ")}</small>
                </div>
              </div>
            )}
          </div>
        </section>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("nav.personalize")}</h2>
          <div className="instance-personalization">
            <label className="ce-row">
              <span>{t("instance.icon")}</span>
              <CeSelect
                className="ce-field"
                aria-label={t("instance.iconLabel")}
                value={metadata.icon}
                disabled={metadataWritesDisabled}
                title={metadataReadOnly ? metadataReadOnlyMessage : undefined}
                onChange={(event) =>
                  void updateMetadata(
                    { icon: event.target.value as InstanceMetadata["icon"] },
                    t("instance.iconSaved"),
                  )
                }
              >
                <option value="auto">{t("ui.auto")}</option>
                <option value="grass">{t("instance.grass")}</option>
                <option value="forge">Forge</option>
                <option value="neoforge">NeoForge</option>
                <option value="command">{t("instance.command")}</option>
                <option value="steve">Steve</option>
              </CeSelect>
            </label>
            <label className="ce-row">
              <span>{t("instance.category")}</span>
              <CeSelect
                className="ce-field"
                aria-label={t("instance.categoryLabel")}
                value={metadata.category}
                disabled={metadataWritesDisabled}
                title={metadataReadOnly ? metadataReadOnlyMessage : undefined}
                onChange={(event) =>
                  void updateMetadata(
                    {
                      category: event.target
                        .value as InstanceMetadata["category"],
                    },
                    t("instance.categorySaved"),
                  )
                }
              >
                <option value="auto">{t("ui.auto")}</option>
                <option value="vanilla">{t("ui.vanilla")}</option>
                <option value="forge">Forge</option>
                <option value="neoforge">NeoForge</option>
                <option value="fabric">Fabric</option>
                <option value="quilt">Quilt</option>
              </CeSelect>
            </label>
            <div className="ce-actions">
              <button
                className="ce-button"
                ref={renameButton}
                disabled={renameDisabled || renameOpen || renameWorking}
                title={
                  !native
                    ? t("instance.renameDesktop")
                    : t("instance.renameFolder")
                }
                onClick={() => {
                  if (
                    renameAvailability.current ||
                    renameWorking ||
                    renameOpen ||
                    metadataScope.current !== currentMetadataScope
                  )
                    return;
                  setRenameDialog({
                    scope: currentMetadataScope,
                    draft: instance.id,
                    plan: null,
                    error: "",
                  });
                }}
              >
                {t("instance.rename")}
              </button>
              <button
                className="ce-button"
                ref={descriptionButton}
                disabled={metadataWritesDisabled}
                title={metadataReadOnly ? metadataReadOnlyMessage : undefined}
                onClick={() => {
                  if (
                    metadataWritesDisabled ||
                    metadataWorkingRef.current ||
                    currentMetadataScope !== metadataScope.current ||
                    !metadataRevision
                  )
                    return;
                  setMetadataFailure(null);
                  setDescriptionDialog({
                    scope: currentMetadataScope,
                    draft: metadata.description,
                    revision: metadataRevision,
                  });
                }}
              >
                {t("instance.descriptionEdit")}
              </button>
              <button
                className="ce-button"
                disabled={metadataWritesDisabled}
                title={metadataReadOnly ? metadataReadOnlyMessage : undefined}
                onClick={() =>
                  void updateMetadata(
                    { favorite: !metadata.favorite },
                    metadata.favorite
                      ? t("instance.favoriteRemoved")
                      : t("instance.favoriteAdded"),
                  )
                }
              >
                {metadata.favorite
                  ? t("instance.removeFavorite")
                  : t("instance.addFavorite")}
              </button>
            </div>
            {(metadataReadOnly || metadataError) && !descriptionOpen && (
              <div
                className={`ce-instance-metadata-status ${metadataError ? "is-error" : ""}`}
              >
                <span role={metadataError ? "alert" : undefined}>
                  {metadataError || metadataReadOnlyMessage}
                </span>
                <button
                  className="ce-button"
                  disabled={metadataWorking}
                  onClick={() => void reloadMetadata()}
                >
                  {metadataActivity === "read"
                    ? t("ui.reading")
                    : t("ui.reread")}
                </button>
              </div>
            )}
          </div>
        </section>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("instance.shortcuts")}</h2>
          <div className="ce-actions">
            <button className="ce-button" onClick={() => onOpen("instance")}>
              {t("instance.folder")}
            </button>
            <button className="ce-button" onClick={() => onOpen("saves")}>
              {t("instance.savesFolder")}
            </button>
            <button className="ce-button" onClick={() => onOpen("mods")}>
              {t("instance.modsFolder")}
            </button>
          </div>
        </section>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("instance.advanced")}</h2>
          <div className="ce-actions advanced-actions">
            <button
              className="ce-button"
              disabled
              title={t("common.unavailable")}
            >
              {t("instance.exportScript")}
            </button>
            <button
              className="ce-button"
              disabled={disabled}
              onClick={onTestLaunch}
            >
              {t("instance.inspect")}
            </button>
            <button
              className="ce-button"
              disabled={!native || disabled || !!mutationDisabled}
              onClick={onInspect}
            >
              {t("inspection.open")}
            </button>
            <button
              className="ce-button"
              disabled={!native || logsDisabled}
              onClick={onLogs}
            >
              {t("instanceLogs.open")}
            </button>
            <button
              className="ce-button"
              disabled={!native || disabled || !!mutationDisabled}
              onClick={onInspect}
              title={t("inspection.repairInspect")}
            >
              {t("instance.completeFiles")}
            </button>
            <button
              className="ce-button"
              disabled
              title={t("common.unavailable")}
            >
              {t("ui.reset")}
            </button>
            <InstanceDelete
              id={instance.id}
              api={api}
              scopeKey={`${rootScope}:${settings.revision || ""}`}
              native={native}
              disabled={
                disabled ||
                !!mutationDisabled ||
                metadataWorking ||
                !!renameOpen ||
                !!renameWorking ||
                !!descriptionOpen
              }
              onTaskStart={onTaskStart}
            />
            <button
              className="ce-button"
              disabled
              title={t("common.unavailable")}
            >
              {t("instance.patchCore")}
            </button>
          </div>
        </section>
        {descriptionOpen && descriptionDialog && (
          <div
            className="modal-shade rd-name-shade"
            onMouseDown={(event) => {
              if (event.target === event.currentTarget) closeDescription();
            }}
          >
            <form
              className="rd-name-dialog ce-instance-description-dialog"
              role="dialog"
              aria-modal="true"
              aria-busy={metadataWorking}
              aria-labelledby="ce-instance-description-title"
              aria-describedby="ce-instance-description-hint"
              onSubmit={(event) => {
                event.preventDefault();
                if (descriptionDialog.scope !== metadataScope.current) return;
                void updateMetadata(
                  { description: descriptionDialog.draft },
                  descriptionDialog.draft
                    ? t("instance.descriptionSaved")
                    : t("instance.descriptionCleared"),
                  descriptionDialog.revision,
                );
              }}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  closeDescription();
                }
                if (event.key === "Tab") {
                  event.stopPropagation();
                  const controls = [
                    ...event.currentTarget.querySelectorAll<HTMLElement>(
                      "textarea:not(:disabled), button:not(:disabled)",
                    ),
                  ];
                  const first = controls[0],
                    last = controls[controls.length - 1];
                  if (!first) event.preventDefault();
                  else if (event.shiftKey && document.activeElement === first) {
                    event.preventDefault();
                    last?.focus();
                  } else if (
                    !event.shiftKey &&
                    document.activeElement === last
                  ) {
                    event.preventDefault();
                    first.focus();
                  }
                }
              }}
            >
              <h2 id="ce-instance-description-title">
                {t("instance.descriptionEdit")}
              </h2>
              <textarea
                autoFocus
                className="ce-field"
                aria-label={t("instance.description")}
                maxLength={4096}
                rows={4}
                value={descriptionDialog.draft}
                disabled={metadataWritesDisabled}
                onChange={(event) => {
                  const draft = event.target.value;
                  setDescriptionDialog((old) =>
                    old?.scope === currentMetadataScope
                      ? { ...old, draft }
                      : old,
                  );
                  setMetadataFailure(null);
                }}
              />
              <p id="ce-instance-description-hint">
                {t("instance.descriptionHelp")}
              </p>
              {metadataError && (
                <>
                  <p className="rd-name-error" role="alert">
                    {metadataError}
                  </p>
                  <p>{t("instance.descriptionReloadHelp")}</p>
                </>
              )}
              <div className="rd-name-actions">
                <button
                  className="ce-button"
                  type="submit"
                  disabled={metadataWritesDisabled}
                >
                  {metadataActivity === "read"
                    ? t("ui.reading")
                    : metadataWorking
                      ? t("ui.saving")
                      : t("ui.confirm")}
                </button>
                {metadataError && (
                  <button
                    className="ce-button"
                    type="button"
                    disabled={metadataWorking}
                    onClick={() => void reloadMetadata()}
                  >
                    {metadataActivity === "read"
                      ? t("ui.reading")
                      : t("ui.reread")}
                  </button>
                )}
                <button
                  className="ce-button"
                  type="button"
                  disabled={metadataWorking}
                  onClick={closeDescription}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </form>
          </div>
        )}
        {renameOpen && renameDialog && (
          <div
            className="modal-shade rd-name-shade"
            onMouseDown={(event) => {
              if (event.target === event.currentTarget) closeRename();
            }}
          >
            <form
              className="rd-name-dialog ce-instance-rename-dialog"
              role="dialog"
              aria-modal="true"
              aria-busy={renameWorking}
              aria-labelledby="ce-instance-rename-title"
              aria-describedby="ce-instance-rename-hint"
              onSubmit={(event) => {
                event.preventDefault();
                void submitRename();
              }}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  closeRename();
                }
                if (event.key === "Tab") {
                  event.stopPropagation();
                  const controls = [
                    ...event.currentTarget.querySelectorAll<HTMLElement>(
                      "input:not(:disabled), button:not(:disabled)",
                    ),
                  ];
                  const first = controls[0],
                    last = controls[controls.length - 1];
                  if (!first) event.preventDefault();
                  else if (event.shiftKey && document.activeElement === first) {
                    event.preventDefault();
                    last?.focus();
                  } else if (
                    !event.shiftKey &&
                    document.activeElement === last
                  ) {
                    event.preventDefault();
                    first.focus();
                  }
                }
              }}
            >
              <h2 id="ce-instance-rename-title">{t("instance.rename")}</h2>
              <input
                autoFocus
                className="ce-field"
                aria-label={t("instance.newName")}
                value={renameDialog.draft}
                disabled={
                  renameActivity?.scope === currentMetadataScope &&
                  renameActivity.kind === "start"
                }
                onChange={(event) => {
                  const current = renameDialogRef.current;
                  if (
                    !current ||
                    currentMetadataScope !== metadataScope.current ||
                    current.scope !== currentMetadataScope ||
                    current.scope !== metadataScope.current ||
                    (renameOperation.current?.scope === current.scope &&
                      renameOperation.current.kind === "start")
                  )
                    return;
                  setRenameDialog({
                    ...current,
                    draft: event.target.value,
                    plan: null,
                    error: "",
                  });
                }}
              />
              <p id="ce-instance-rename-hint">{t("instance.renameHelp")}</p>
              {(renameDialog.error ||
                (renameDialog.draft !== instance.id && renameNameError)) && (
                <p className="rd-name-error" role="alert">
                  {renameDialog.error || renameNameError}
                </p>
              )}
              {renameDialog.plan && (
                <div className="ce-instance-rename-plan">
                  <p>
                    {renameDialog.plan.id} → {renameDialog.plan.new_name}
                  </p>
                  <p>
                    {t("instance.dependentsCount", {
                      count: formatNumber(
                        renameDialog.plan.dependent_instances.length,
                      ),
                    })}
                  </p>
                  {!!renameDialog.plan.dependent_instances.length && (
                    <ul>
                      {renameDialog.plan.dependent_instances
                        .slice(0, 5)
                        .map((id) => (
                          <li key={id}>{id}</li>
                        ))}
                      {renameDialog.plan.dependent_instances.length > 5 && (
                        <li>
                          {t("instance.moreVersions", {
                            count: formatNumber(
                              renameDialog.plan.dependent_instances.length - 5,
                            ),
                          })}
                        </li>
                      )}
                    </ul>
                  )}
                </div>
              )}
              <div className="rd-name-actions">
                <button
                  type="submit"
                  className="ce-button primary"
                  disabled={
                    renameDisabled || renameWorking || !!renameNameError
                  }
                >
                  {renameWorking
                    ? renameActivity?.kind === "start"
                      ? t("ui.submitting")
                      : t("ui.checking")
                    : renameDialog.plan
                      ? t("instance.confirmRename")
                      : t("instance.checkRename")}
                </button>
                <button
                  type="button"
                  className="ce-button"
                  disabled={
                    renameActivity?.scope === currentMetadataScope &&
                    renameActivity.kind === "start"
                  }
                  onClick={closeRename}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </form>
          </div>
        )}
      </>
    );
  if (section === "settings") {
    const allocation = mode === "global" ? settings.memory_gib : memory;
    const used = system
      ? system.total_memory_bytes - system.available_memory_bytes
      : 0;
    const gib = (bytes: number) =>
      formatNumber(bytes / 1073741824, {
        minimumFractionDigits: 1,
        maximumFractionDigits: 1,
      });
    return (
      <>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("instance.launchOptions")}</h2>
          <div className="instance-launch-fields">
            <IsolationField
              settings={settings}
              instanceId={instance.id}
              onSave={onSave}
              scope={`${rootScope}:${settings.root}:${settings.revision}:${instance.id}:${section}`}
              disabled={!native || disabled || !!mutationDisabled}
              onNotify={onNotify}
            />
            <GameTitleField
              value={settings.window_title_overrides?.[instance.id]}
              inherited
              scope={`${rootScope}:${settings.root}:${settings.revision}:${instance.id}:${section}`}
              disabled={!native || disabled || !!mutationDisabled}
              onNotify={onNotify}
              onSave={async (title) => {
                const titles = { ...settings.window_title_overrides };
                if (title === undefined) delete titles[instance.id];
                else titles[instance.id] = title;
                await onSave({ ...settings, window_title_overrides: titles });
              }}
            />
            {argumentField("custom_info")}
            <JavaSelect
              instance={instance}
              settings={settings}
              api={api}
              native={native}
              disabled={disabled || !!mutationDisabled}
              onSave={onSave}
              onNotify={onNotify}
            />
          </div>
        </section>
        <section className="ce-card ce-memory-card">
          <h2 className="ce-card-title">{t("instance.memory")}</h2>
          {system &&
            allocation * 1073741824 > system.available_memory_bytes && (
              <div className="ce-memory-warning">
                {t("instance.memoryWarning")}
              </div>
            )}
          <label className="ce-memory-mode">
            <input
              type="radio"
              name="instance-memory"
              checked={mode === "global"}
              disabled={disabled}
              onChange={() => {
                setMode("global");
                void saveMemory("global");
              }}
            />
            {t("ui.followGlobal")}
          </label>
          <label className="ce-memory-mode">
            <input
              type="radio"
              name="instance-memory"
              checked={false}
              readOnly
              disabled
              title={t("common.unavailable")}
            />
            {t("instance.memoryAuto")}
          </label>
          <div className="ce-memory-custom">
            <label className="ce-memory-mode">
              <input
                type="radio"
                name="instance-memory"
                checked={mode === "custom"}
                disabled={disabled}
                onChange={() => {
                  setMode("custom");
                  void saveMemory("custom");
                }}
              />
              {t("ui.custom")}
            </label>
            <input
              type="range"
              aria-label={t("instance.memoryLabel")}
              min="2"
              max={Math.max(14, memory)}
              value={memory}
              disabled={disabled || mode !== "custom"}
              onChange={(e) => setMemory(Number(e.target.value))}
              onPointerUp={() => void saveMemory()}
              onKeyUp={() => void saveMemory()}
              onBlur={() => {
                if (mode === "custom") void saveMemory();
              }}
            />
          </div>
          <div className="ce-memory-labels">
            <span>{t("instance.usedMemory")}</span>
            <span>{t("instance.allocatedMemory")}</span>
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
                : t("instance.memoryUnavailable")}
            </span>
            <span>
              {formatNumber(allocation, {
                minimumFractionDigits: 1,
                maximumFractionDigits: 1,
              })}{" "}
              GiB
              {system
                ? t("instance.availableMemory", {
                    memory: gib(system.available_memory_bytes),
                  })
                : ""}
            </span>
          </div>
        </section>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("settings.advanced")}</h2>
          <div className="instance-launch-fields">
            {argumentField("jvm_args")}
            {argumentField("game_args")}
            <p className="ce-game-field-hint">{t("settings.argumentsHint")}</p>
          </div>
        </section>
        <section className="ce-card">
          <h2 className="ce-card-title">{t("nav.server")}</h2>
          <div className="instance-launch-fields">
            <label className="ce-row">
              <span>{t("instance.authentication")}</span>
              <CeSelect
                className="ce-field"
                disabled
                title={t("common.unavailable")}
              >
                <option>{t("ui.unrestricted")}</option>
              </CeSelect>
            </label>
            <label className="ce-row">
              <span>{t("instance.autoServer")}</span>
              <input
                className="ce-field"
                disabled
                title={t("common.unavailable")}
              />
            </label>
          </div>
        </section>
      </>
    );
  }
  if (section === "server")
    return (
      <ServerPanel
        id={instance.id}
        api={api}
        generation={`${rootScope}:${settings.revision || ""}`}
      />
    );
  if (section === "modify" || section === "export")
    return (
      <InstanceOperations
        instance={instance}
        section={section}
        api={api}
        scopeKey={`${rootScope}:${settings.revision || ""}`}
        native={native}
        busy={disabled || !!mutationDisabled}
        onTaskStart={onTaskStart}
        onNotify={onNotify}
      />
    );
  return (
    <ResourcePanel
      id={instance.id}
      section={section}
      api={api}
      onOpen={onOpen}
      onNotify={onNotify}
      onResourceDetails={onResourceDetails}
      disabled={disabled}
      mutationDisabled={mutationDisabled}
      instance={instance}
      scopeKey={`${rootScope}:${settings.revision || ""}`}
      generation={JSON.stringify([settings.revision || "", resourceRefreshKey])}
      native={native}
      onTaskStart={onTaskStart}
    />
  );
}
type ServerEntry = { name: string; ip: string; icon?: string };
function ServerPanel({
  id,
  api,
  generation,
}: {
  id: string;
  api: Api;
  generation: string;
}) {
  const [servers, setServers] = useState<ServerEntry[]>([]),
    [error, setError] = useState(""),
    [refresh, setRefresh] = useState(0);
  useEffect(() => {
    let live = true;
    setServers([]);
    setError("");
    api<ServerEntry[]>("instance_servers", { id })
      .then((v) => {
        if (live) setServers(v);
      })
      .catch((e) => {
        if (live) setError(String(e));
      });
    return () => {
      live = false;
    };
  }, [id, api, refresh, generation]);
  return (
    <div className="ce-server-panel">
      <section className="ce-card">
        <h2 className="ce-card-title">{t("server.actions")}</h2>
        <div className="ce-actions">
          <button
            className="ce-button primary"
            onClick={() => setRefresh((v) => v + 1)}
            title={t("server.refreshHelp")}
          >
            {t("server.refresh")}
          </button>
          <button
            className="ce-button"
            disabled
            title={t("common.unavailable")}
          >
            {t("server.add")}
          </button>
        </div>
      </section>
      {error ? (
        <p className="ce-empty">{error}</p>
      ) : !servers.length ? (
        <p className="ce-empty">{t("server.empty")}</p>
      ) : (
        servers.map((v, i) => (
          <section className="ce-card ce-server-row" key={v.ip + i}>
            {v.icon ? (
              <img src={v.icon} alt="" />
            ) : (
              <span className="ce-server-icon">
                <Server size={25} />
              </span>
            )}
            <div className="ce-server-info">
              <strong>{v.name}</strong>
              <small>
                <Signal size={14} />
                {t("server.unchecked")}
              </small>
            </div>
            <span className="ce-server-message">{v.ip}</span>
            <button
              disabled
              title={t("common.unavailable")}
              aria-label={t("server.join", { name: v.name })}
            >
              <Play size={16} />
            </button>
            <button
              disabled
              title={t("common.unavailable")}
              aria-label={t("server.settings", { name: v.name })}
            >
              <SettingsIcon size={16} />
            </button>
          </section>
        ))
      )}
    </div>
  );
}
