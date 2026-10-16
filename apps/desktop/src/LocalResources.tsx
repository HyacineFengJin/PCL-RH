import { t, formatNumber } from "./i18n";
/** Existing CE local-resource list, selection and local file operations.
 * Update inspection/confirmation is owned by the separate update components. */
import { useContext, useEffect, useRef, useState } from "react";
import { LauncherNavigationContext } from "./useLauncherPreferences";
import { resourceBrowseRequest } from "./resourceBrowse";
import { ResourceFavorite } from "./LauncherFavorites";
import { useResourceInfoExport } from "./useResourceInfoExport";
import {
  Box,
  Search,
  ArrowDownUp,
  Info,
  FolderOpen,
  CircleMinus,
  CircleCheck,
  Trash2,
  Heart,
  Share2,
  Upload,
  X,
} from "lucide-react";
import type { Api } from "./types";
import {
  ResourceUpdates,
  ResourceUpdateStatus,
  ResourceUpdateUndo,
} from "./ResourceUpdates";
import { useResourceUpdates } from "./useResourceUpdates";
import type { ResourceFile, ResourceUpdateTarget } from "./resourceUpdateTypes";

export type LocalResourceDetails = {
  name: string;
  path: string;
  enabled: boolean;
  version?: string;
  description?: string;
  file_name?: string;
  fingerprint?: string | null;
  icon?: string;
  kind: string;
};
type Resource = Omit<LocalResourceDetails, "kind">;
type RemovedResourceOperation = {
  id: string;
  files: string[];
  created_at: number;
};
type ResourceWriteResult = {
  changed: number;
  undo_id: string | null;
  message: string;
};
type ResourceImportResult = {
  status: "complete" | "cancelled" | "unavailable";
  changed: number;
  message?: string;
  undo_id?: string | null;
};
type ResourceRemovalChoice = {
  files: ResourceFile[];
  names: string[];
};
function writableResourceFile(resource: Resource): ResourceFile | null {
  return resource.file_name && resource.fingerprint
    ? {
        file_name: resource.file_name,
        fingerprint: resource.fingerprint,
      }
    : null;
}
export function ResourcePanel({
  id,
  section,
  api,
  onOpen,
  onNotify,
  onResourceDetails,
  disabled,
  mutationDisabled = false,
  instance,
  scopeKey,
  generation,
  native,
  onTaskStart,
}: {
  id: string;
  section: string;
  api: Api;
  onOpen: (s: string) => void;
  onNotify: (s: string) => void;
  onResourceDetails?: (resource: LocalResourceDetails) => void;
  disabled: boolean;
  mutationDisabled?: boolean;
  instance: ResourceUpdateTarget;
  scopeKey: string;
  generation: string;
  native: boolean;
  onTaskStart: (id: string) => void;
}) {
  const navigation = useContext(LauncherNavigationContext);
  const browseRequest = resourceBrowseRequest(
    section,
    instance.minecraft_version,
    instance.loader,
  );
  const hideUpdates = navigation.isHidden("feature.mod_updates");
  const [entries, setEntries] = useState<Resource[]>([]),
    [query, setQuery] = useState(""),
    [error, setError] = useState(""),
    [loading, setLoading] = useState(true),
    [descending, setDescending] = useState(false),
    [filter, setFilter] = useState<"all" | "updates">("all"),
    [selected, setSelected] = useState<string[]>([]),
    [detail, setDetail] = useState<Resource | null>(null),
    [working, setWorking] = useState(""),
    [actionError, setActionError] = useState(""),
    [recoveryError, setRecoveryError] = useState(""),
    [removed, setRemoved] = useState<RemovedResourceOperation[]>([]),
    [resourceGeneration, setResourceGeneration] = useState(0),
    [removalChoice, setRemovalChoice] = useState<ResourceRemovalChoice | null>(
      null,
    );
  const infoExport = useResourceInfoExport({
    api,
    contextKey: `${scopeKey}:${id}:${section}:${generation}:${resourceGeneration}`,
    native,
    id,
    kind: section,
    onNotify,
  });
  useEffect(() => {
    if (hideUpdates) setFilter("all");
  }, [hideUpdates]);
  const alive = useRef(false),
    scopeGeneration = useRef(0),
    readGeneration = useRef(0),
    workingRef = useRef(false),
    removalDialog = useRef<HTMLDivElement>(null),
    currentIdentity = useRef({ id, section, api, scopeKey, generation });
  // Returning to equal IDs creates a new owner; earlier saved actions stay retired.
  if (
    currentIdentity.current.id !== id ||
    currentIdentity.current.section !== section ||
    currentIdentity.current.api !== api ||
    currentIdentity.current.scopeKey !== scopeKey ||
    currentIdentity.current.generation !== generation
  )
    currentIdentity.current = { id, section, api, scopeKey, generation };
  const renderedIdentity = currentIdentity.current;
  const admission = useRef({ disabled, mutationDisabled });
  admission.current = { disabled, mutationDisabled };
  const writableKind = ["mods", "resourcepacks", "shaderpacks"].includes(
    section,
  );
  function isCurrent(scope: number) {
    return (
      alive.current &&
      scopeGeneration.current === scope &&
      currentIdentity.current === renderedIdentity
    );
  }
  async function readResources(scope: number, clearSelection = true) {
    if (!isCurrent(scope)) return;
    const request = ++readGeneration.current;
    setLoading(true);
    setError("");
    setRecoveryError("");
    const [resourcesResult, removedResult] = await Promise.allSettled([
      api<Resource[]>("instance_resources", { id, kind: section }),
      writableKind
        ? api<RemovedResourceOperation[]>("resource_removed", {
            id,
            kind: section,
          })
        : Promise.resolve([] as RemovedResourceOperation[]),
    ]);
    if (!isCurrent(scope) || readGeneration.current !== request) return;
    if (resourcesResult.status === "fulfilled") {
      setEntries(resourcesResult.value);
      setResourceGeneration((current) => current + 1);
      if (clearSelection) setSelected([]);
    } else {
      setError(String(resourcesResult.reason));
    }
    if (removedResult.status === "fulfilled") {
      setRemoved(
        [...removedResult.value].sort((a, b) => b.created_at - a.created_at),
      );
    } else {
      setRecoveryError(String(removedResult.reason));
    }
    setLoading(false);
  }
  useEffect(() => {
    const scope = ++scopeGeneration.current;
    alive.current = true;
    workingRef.current = false;
    setWorking("");
    setLoading(true);
    setEntries([]);
    setError("");
    setQuery("");
    setSelected([]);
    setFilter("all");
    setDetail(null);
    setActionError("");
    setRecoveryError("");
    setRemoved([]);
    setRemovalChoice(null);
    void readResources(scope);
    return () => {
      alive.current = false;
      ++scopeGeneration.current;
      ++readGeneration.current;
    };
  }, [id, section, api, scopeKey, generation]);
  useEffect(() => {
    if (!detail) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") setDetail(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [detail]);
  useEffect(() => {
    if (!removalChoice) return;
    const previous = document.activeElement as HTMLElement | null;
    removalDialog.current?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => {
      if (previous?.isConnected) previous.focus();
    };
  }, [removalChoice]);
  const mutationBlocked =
    disabled || mutationDisabled || !!working || loading || !writableKind;
  const updates = useResourceUpdates({
    api,
    scopeKey,
    instance,
    generation: `${generation}:${resourceGeneration}`,
    files: entries
      .map(writableResourceFile)
      .filter((file): file is ResourceFile => file !== null),
    enabled: section === "mods" && !loading && !error && !recoveryError,
    native,
    disabled: mutationBlocked,
    onTaskStart,
    onNotify,
  });
  const writesDisabled =
    mutationBlocked ||
    !!error ||
    !!recoveryError ||
    updates.busy ||
    !!updates.choice;
  const updateFor = (resource: Resource) =>
    !hideUpdates
      ? updates.result?.entries.find(
          (entry) =>
            entry.file_name === resource.file_name &&
            entry.fingerprint === resource.fingerprint,
        )
      : undefined;
  const filtered = entries
    .filter((v) =>
      [v.name, v.file_name, v.description].some((value) =>
        value?.toLowerCase().includes(query.toLowerCase()),
      ),
    )
    .filter(
      (v) =>
        filter !== "updates" || updateFor(v)?.status === "update_available",
    )
    .sort(
      (a, b) => a.name.localeCompare(b.name, "zh-CN") * (descending ? -1 : 1),
    );
  const selectedEntries = entries.filter((entry) =>
    selected.includes(entry.path),
  );
  // Bookmark identity is authoritative inspection data, independent of whether
  // update actions are hidden. A filename alone never identifies a publisher.
  const favoriteProjects = selectedEntries.map(
    (resource) =>
      updates.result?.entries.find(
        (entry) =>
          entry.file_name === resource.file_name &&
          entry.fingerprint === resource.fingerprint,
      )?.project_id,
  );
  const recognizedSelection =
    section === "mods" &&
    selectedEntries.length > 0 &&
    favoriteProjects.every((id): id is string => !!id);
  const favoriteContext = JSON.stringify([
    scopeKey,
    id,
    section,
    generation,
    resourceGeneration,
    selectedEntries.map((resource) => [
      resource.file_name,
      resource.fingerprint,
    ]),
  ]);
  const allVisibleSelected =
    filtered.length > 0 &&
    filtered.every((entry) => selected.includes(entry.path));
  function toggleSelection(path: string) {
    setSelected((old) =>
      old.includes(path) ? old.filter((v) => v !== path) : [...old, path],
    );
  }
  function openDetails(resource: Resource) {
    if (onResourceDetails) onResourceDetails({ ...resource, kind: section });
    else setDetail(resource);
  }
  function filesFor(resources: Resource[]): ResourceFile[] | null {
    const files = resources.map(writableResourceFile);
    return resources.length > 0 && files.every((file) => file !== null)
      ? (files as ResourceFile[])
      : null;
  }
  async function writeResources(
    status: string,
    operation: () => Promise<ResourceWriteResult | ResourceImportResult>,
    recovering = false,
  ) {
    const scope = scopeGeneration.current;
    if (
      !isCurrent(scope) ||
      workingRef.current ||
      admission.current.disabled ||
      admission.current.mutationDisabled ||
      (recovering ? mutationBlocked : writesDisabled)
    )
      return;
    workingRef.current = true;
    setWorking(status);
    setActionError("");
    setRemovalChoice(null);
    try {
      const result = await operation();
      if (!isCurrent(scope)) return;
      if ("status" in result && result.status === "cancelled") return;
      if ("status" in result && result.status === "unavailable") {
        const message = result.message || t("local.pickerUnavailable");
        setActionError(message);
        onNotify(message);
        return;
      }
      setSelected([]);
      setDetail(null);
      setWorking(t("local.rereading"));
      await readResources(scope, false);
      if (isCurrent(scope))
        onNotify(
          result.message ||
            t("local.processed", { count: formatNumber(result.changed) }),
        );
    } catch (e) {
      if (isCurrent(scope)) {
        const message = String(e);
        setActionError(message);
        onNotify(message);
      }
    } finally {
      if (isCurrent(scope)) {
        workingRef.current = false;
        setWorking("");
      }
    }
  }
  function setEnabled(resources: Resource[], enabled: boolean) {
    const files = filesFor(resources);
    if (section !== "mods" || !files) return;
    void writeResources(
      enabled ? t("local.enabling") : t("local.disabling"),
      () =>
        api<ResourceWriteResult>("resource_set_enabled", {
          id,
          kind: section,
          files,
          enabled,
        }),
    );
  }
  function importResources() {
    void writeResources(t("local.importing"), () =>
      api<ResourceImportResult>("resource_import", { id, kind: section }),
    );
  }
  function chooseRemoval(resources: Resource[]) {
    const files = filesFor(resources);
    if (
      writesDisabled ||
      workingRef.current ||
      admission.current.disabled ||
      admission.current.mutationDisabled ||
      !isCurrent(scopeGeneration.current) ||
      !files
    )
      return;
    setRemovalChoice({
      files,
      names: resources.map((resource) => resource.name),
    });
  }
  function refreshResources() {
    if (!isCurrent(scopeGeneration.current) || workingRef.current || loading)
      return;
    setActionError("");
    void readResources(scopeGeneration.current);
  }
  const browseButton = (
    <button
      className="ce-button"
      disabled={!browseRequest || !navigation.browseResources}
      title={!browseRequest ? t("local.infoKinds") : undefined}
      onClick={() => {
        if (isCurrent(scopeGeneration.current) && browseRequest)
          navigation.browseResources?.(browseRequest);
      }}
    >
      {t("local.downloadNew")}
    </button>
  );
  const refreshButton = (
    <button
      className="ce-button"
      disabled={loading || !!working}
      onClick={refreshResources}
    >
      {t("ui.refresh")}
    </button>
  );
  const selectedWritable = !!filesFor(selectedEntries),
    selectedUpdatable =
      section === "mods" &&
      selectedEntries.length > 0 &&
      selectedEntries.every(
        (entry) => updateFor(entry)?.status === "update_available",
      ),
    updateCount = updates.result?.entries.filter(
      (entry) => entry.status === "update_available",
    ).length,
    updateBlocked = mutationBlocked || !!error || !!recoveryError,
    latestRemoval = removed[0],
    operationFeedback = (
      <>
        {working && (
          <p className="ce-resource-operation-status" role="status">
            {working}
          </p>
        )}
        {(actionError || recoveryError) && (
          <div className="ce-resource-operation-error" role="alert">
            <span>{actionError || recoveryError}</span>
            {recoveryError && writableKind && (
              <button
                className="ce-button"
                disabled={mutationBlocked}
                onClick={() =>
                  void writeResources(
                    t("local.recovering"),
                    () =>
                      api<ResourceWriteResult>("resource_recover", {
                        id,
                        kind: section,
                      }),
                    true,
                  )
                }
              >
                {t("local.recover")}
              </button>
            )}
          </div>
        )}
        {section === "mods" && (
          <ResourceUpdateUndo updates={updates} disabled={updateBlocked} />
        )}
      </>
    ),
    toolbar = (
      <section className="ce-card resource-toolbar">
        <button className="ce-button primary" onClick={() => onOpen(section)}>
          {t("common.openFolder")}
        </button>
        <button
          className="ce-button"
          disabled={writesDisabled}
          title={!writableKind ? t("common.unavailable") : undefined}
          onClick={importResources}
        >
          {t("local.installFile")}
        </button>
        {browseButton}
        {refreshButton}
        <button
          className="ce-button"
          disabled={loading || !!error || !filtered.length}
          onClick={() =>
            setSelected((old) =>
              allVisibleSelected
                ? old.filter((path) => !filtered.some((v) => v.path === path))
                : [...new Set([...old, ...filtered.map((v) => v.path)])],
            )
          }
        >
          {allVisibleSelected ? t("ui.deselectAll") : t("ui.selectAll")}
        </button>
        <button
          className="ce-button"
          disabled={
            !native ||
            loading ||
            !!error ||
            infoExport.busy ||
            !writableKind ||
            !(selectedEntries.length ? selectedEntries : filtered).every(
              writableResourceFile,
            ) ||
            !(selectedEntries.length ? selectedEntries : filtered).length
          }
          title={t("local.infoExportHelp")}
          onClick={() => {
            const files = filesFor(
              selectedEntries.length ? selectedEntries : filtered,
            );
            if (isCurrent(scopeGeneration.current) && files)
              void infoExport.exportInfo(files);
          }}
        >
          {t("local.exportInfo")}
        </button>
        {latestRemoval && writableKind && (
          <button
            className="ce-button"
            disabled={writesDisabled}
            title={t("local.restoreCount", {
              count: formatNumber(latestRemoval.files.length),
            })}
            onClick={() =>
              void writeResources(t("local.restoring"), () =>
                api<ResourceWriteResult>("resource_restore", {
                  id,
                  kind: section,
                  operationId: latestRemoval.id,
                }),
              )
            }
          >
            {t("trash.undo")}
          </button>
        )}
      </section>
    ),
    removalConfirmation = removalChoice && (
      <div
        className="modal-shade rd-name-shade"
        onClick={() => setRemovalChoice(null)}
      >
        <div
          className="rd-name-dialog ce-resource-remove-dialog"
          ref={removalDialog}
          role="dialog"
          aria-modal="true"
          aria-labelledby="ce-resource-remove-title"
          aria-describedby="ce-resource-remove-description"
          onClick={(event) => event.stopPropagation()}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              setRemovalChoice(null);
            }
            if (event.key === "Tab") {
              const controls = [
                ...event.currentTarget.querySelectorAll<HTMLButtonElement>(
                  "button:not(:disabled)",
                ),
              ];
              const first = controls[0],
                last = controls[controls.length - 1];
              if (event.shiftKey && document.activeElement === first) {
                event.preventDefault();
                last?.focus();
              } else if (!event.shiftKey && document.activeElement === last) {
                event.preventDefault();
                first?.focus();
              }
            }
          }}
        >
          <h2 id="ce-resource-remove-title">{t("local.deleteResources")}</h2>
          <p id="ce-resource-remove-description">
            {t("local.removeConfirm", {
              count: formatNumber(removalChoice.files.length),
            })}
          </p>
          <ul className="ce-resource-remove-files">
            {removalChoice.files.map((file, index) => (
              <li key={file.file_name}>
                <strong>{removalChoice.names[index]}</strong>
                <small>{file.file_name}</small>
              </li>
            ))}
          </ul>
          <div className="rd-name-actions">
            <button
              className="ce-button"
              onClick={() => setRemovalChoice(null)}
            >
              {t("common.cancel")}
            </button>
            <button
              className="ce-button primary"
              disabled={writesDisabled}
              onClick={() =>
                void writeResources(t("local.deleting"), () =>
                  api<ResourceWriteResult>("resource_remove", {
                    id,
                    kind: section,
                    files: removalChoice.files,
                  }),
                )
              }
            >
              {t("ui.delete")}
            </button>
          </div>
        </div>
      </div>
    );
  if (!loading && !error && !entries.length)
    return (
      <div
        className={
          "ce-local-resources is-empty" + (latestRemoval ? " has-recovery" : "")
        }
      >
        {latestRemoval && toolbar}
        {operationFeedback}
        <div className="ce-state-stage">
          <section className="ce-card ce-state-box">
            <h2>{t("local.empty")}</h2>
            <p>
              {t("local.installHelp")}
              <br />
              {t("local.refreshHelp")}
            </p>
            {!latestRemoval && (
              <div className="ce-actions">
                <button
                  className="ce-button primary"
                  disabled={writesDisabled}
                  title={!writableKind ? t("common.unavailable") : undefined}
                  onClick={importResources}
                >
                  {t("local.installFile")}
                </button>
                {browseButton}
                <button className="ce-button" onClick={() => onOpen(section)}>
                  {t("common.openFolder")}
                </button>
                {refreshButton}
              </div>
            )}
          </section>
        </div>
        {removalConfirmation}
      </div>
    );
  return (
    <div className="ce-local-resources">
      <label className="ce-card ce-searchbar">
        <Search size={17} />
        <input
          placeholder={t("local.searchPlaceholder")}
          aria-label={t("local.search")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </label>
      {toolbar}
      {operationFeedback}
      <section className="ce-card resource-list">
        <div className="resource-list-heading">
          <div className="resource-tabs">
            <button
              className={filter === "all" ? "ce-pill" : ""}
              aria-pressed={filter === "all"}
              onClick={() => setFilter("all")}
            >
              {t("local.allCount", { count: formatNumber(entries.length) })}
            </button>
            {section === "mods" && !hideUpdates && (
              <button
                className={filter === "updates" ? "ce-pill" : ""}
                aria-pressed={filter === "updates"}
                onClick={() => {
                  setFilter("updates");
                  setSelected([]);
                }}
              >
                {t("local.updatable")}
                {updates.phase === "checking"
                  ? t("local.checkingSuffix")
                  : updateCount !== undefined
                    ? ` (${updateCount})`
                    : ""}
              </button>
            )}
          </div>
          <button
            onClick={() => setDescending(!descending)}
            title={
              descending ? t("local.sortDescending") : t("local.sortAscending")
            }
          >
            <ArrowDownUp size={17} />
            {t("local.sortName")}
          </button>
        </div>
        {loading ? (
          <p className="ce-empty">{t("local.reading")}</p>
        ) : error ? (
          <div className="ce-empty ce-resource-read-error" role="alert">
            <p>{error}</p>
            <button
              className="ce-button"
              disabled={!!working}
              onClick={refreshResources}
            >
              {t("ui.reread")}
            </button>
          </div>
        ) : filter === "updates" && !filtered.length ? (
          <ResourceUpdateStatus
            updates={updates}
            emptyOnly
            onRetry={refreshResources}
            disabled={updateBlocked}
            readonlyCount={
              entries.filter((entry) => !writableResourceFile(entry)).length
            }
          />
        ) : !filtered.length ? (
          <p className="ce-empty">{t("local.notFound")}</p>
        ) : (
          <>
            {section === "mods" && !hideUpdates && (
              <ResourceUpdateStatus
                updates={updates}
                onRetry={refreshResources}
                disabled={updateBlocked}
                readonlyCount={
                  entries.filter((entry) => !writableResourceFile(entry)).length
                }
              />
            )}
            <div
              role="listbox"
              aria-label={t("local.resources")}
              aria-multiselectable="true"
            >
              {filtered.map((v) => (
                <div
                  className={
                    "resource-row ce-local-resource-row " +
                    (!v.enabled ? "resource-disabled " : "") +
                    (selected.includes(v.path) ? "is-selected" : "")
                  }
                  key={v.path}
                  role="option"
                  tabIndex={0}
                  aria-selected={selected.includes(v.path)}
                  onClick={() => toggleSelection(v.path)}
                  onKeyDown={(event) => {
                    if (
                      event.target === event.currentTarget &&
                      (event.key === "Enter" || event.key === " ")
                    ) {
                      event.preventDefault();
                      toggleSelection(v.path);
                    }
                  }}
                >
                  <span className="resource-icon">
                    {v.icon ? (
                      <img src={v.icon} alt="" />
                    ) : (
                      <Box size={29} strokeWidth={1.6} />
                    )}
                  </span>
                  <div className="ce-resource-text">
                    <strong>
                      {section === "mods" &&
                      navigation.modDisplayStyle === "file_name"
                        ? v.file_name || v.name
                        : v.name || v.file_name}
                      {updateFor(v)?.status === "update_available" ? (
                        <small className="ce-resource-update-version">
                          {" "}
                          |{" "}
                          {updateFor(v)?.old_version ||
                            v.version ||
                            t("local.installedVersion")}{" "}
                          → {updateFor(v)?.new_version} ↑
                        </small>
                      ) : (
                        (v.version || updateFor(v)?.old_version) && (
                          <small>
                            {" "}
                            | {v.version || updateFor(v)?.old_version}
                          </small>
                        )
                      )}
                      {updateFor(v)?.status === "unknown" && (
                        <small title={updateFor(v)?.reason ?? undefined}>
                          {" "}
                          {t("local.unknownSuffix")}
                        </small>
                      )}
                      {updateFor(v)?.status === "blocked" && (
                        <small title={updateFor(v)?.reason ?? undefined}>
                          {" "}
                          {t("local.blockedSuffix")}
                        </small>
                      )}
                    </strong>
                    <small>
                      {v.file_name || v.name}
                      {v.description ? `: ${v.description}` : ""}
                    </small>
                    {!v.enabled && <small>{t("ui.disabled")}</small>}
                    {writableKind && !writableResourceFile(v) && (
                      <small>{t("ui.readonly")}</small>
                    )}
                  </div>
                  <div
                    className="ce-resource-hover-actions"
                    onClick={(event) => event.stopPropagation()}
                  >
                    <span
                      className="ce-resource-action-tip"
                      data-tooltip={t("ui.details")}
                    >
                      <button
                        aria-label={t("ui.resourceAction", {
                          name: v.name,
                          action: t("ui.details"),
                        })}
                        onClick={() => openDetails(v)}
                      >
                        <Info size={15} />
                      </button>
                    </span>
                    <span
                      className="ce-resource-action-tip"
                      data-tooltip={t("local.openLocation")}
                    >
                      <button
                        aria-label={t("ui.resourceAction", {
                          name: v.name,
                          action: t("local.openFolder"),
                        })}
                        onClick={() => onOpen(section)}
                      >
                        <FolderOpen size={15} />
                      </button>
                    </span>
                    <span
                      className="ce-resource-action-tip"
                      data-tooltip={
                        section !== "mods"
                          ? t("local.modsOnly")
                          : !writableResourceFile(v)
                            ? t("local.identityReadonly")
                            : v.enabled
                              ? t("ui.disable")
                              : t("ui.enable")
                      }
                    >
                      <button
                        disabled={
                          writesDisabled ||
                          section !== "mods" ||
                          !writableResourceFile(v)
                        }
                        aria-label={t("ui.resourceAction", {
                          name: v.name,
                          action: t(v.enabled ? "ui.disable" : "ui.enable"),
                        })}
                        onClick={() => setEnabled([v], !v.enabled)}
                      >
                        {v.enabled ? (
                          <CircleMinus size={15} />
                        ) : (
                          <CircleCheck size={15} />
                        )}
                      </button>
                    </span>
                    <span
                      className="ce-resource-action-tip"
                      data-tooltip={
                        !writableKind
                          ? t("local.deleteUnavailable")
                          : !writableResourceFile(v)
                            ? t("local.identityReadonly")
                            : t("local.deleteRecoverable")
                      }
                    >
                      <button
                        disabled={writesDisabled || !writableResourceFile(v)}
                        aria-label={t("ui.resourceAction", {
                          name: v.name,
                          action: t("ui.delete"),
                        })}
                        onClick={() => chooseRemoval([v])}
                      >
                        <Trash2 size={15} />
                      </button>
                    </span>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </section>
      {selectedEntries.length > 0 && (
        <div
          className="ce-resource-selection-bar"
          aria-label={t("local.selectionActions")}
        >
          <div className="ce-resource-selection-count">
            {t("local.selectedCount", {
              count: formatNumber(selectedEntries.length),
            })}
            {writableKind && !selectedWritable && (
              <span>{t("local.readonlySelectionSuffix")}</span>
            )}
          </div>
          <div className="ce-resource-selection-actions">
            {!hideUpdates && (
              <button
                disabled={writesDisabled || !selectedUpdatable}
                title={
                  !selectedUpdatable ? t("local.notAllUpdatable") : undefined
                }
                onClick={() => {
                  const files = filesFor(selectedEntries);
                  if (files) updates.open(files);
                }}
              >
                <Upload size={16} />
                {t("ui.update")}
              </button>
            )}
            <button
              disabled={
                writesDisabled || section !== "mods" || !selectedWritable
              }
              title={
                section !== "mods"
                  ? t("local.modsOnly")
                  : !selectedWritable
                    ? t("local.selectionReadonly")
                    : undefined
              }
              onClick={() => setEnabled(selectedEntries, true)}
            >
              <CircleCheck size={16} />
              {t("ui.enable")}
            </button>
            <button
              disabled={
                writesDisabled || section !== "mods" || !selectedWritable
              }
              title={
                section !== "mods"
                  ? t("local.modsOnly")
                  : !selectedWritable
                    ? t("local.selectionReadonly")
                    : undefined
              }
              onClick={() => setEnabled(selectedEntries, false)}
            >
              <CircleMinus size={16} />
              {t("ui.disable")}
            </button>
            <ResourceFavorite
              projectId={favoriteProjects[0] || ""}
              projectIds={
                recognizedSelection ? (favoriteProjects as string[]) : []
              }
              supported={
                native &&
                recognizedSelection &&
                !loading &&
                !error &&
                !recoveryError
              }
              contextKey={favoriteContext}
              unavailableKey="favorites.localUnidentified"
            />
            <button
              disabled={!native || infoExport.busy || !selectedWritable}
              title={t("local.infoExportHelp")}
              onClick={() => {
                const files = filesFor(selectedEntries);
                if (isCurrent(scopeGeneration.current) && files)
                  void infoExport.exportInfo(files);
              }}
            >
              <Share2 size={16} />
              {t("local.shareSelected")}
            </button>
            <button
              disabled={writesDisabled || !selectedWritable}
              title={
                !writableKind
                  ? t("common.unavailable")
                  : !selectedWritable
                    ? t("local.selectionReadonly")
                    : t("local.undoable")
              }
              onClick={() => chooseRemoval(selectedEntries)}
            >
              <Trash2 size={16} />
              {t("ui.delete")}
            </button>
            <button onClick={() => setSelected([])}>
              <X size={16} />
              {t("ui.deselect")}
            </button>
          </div>
        </div>
      )}
      {removalConfirmation}
      {updates.choice && (
        <ResourceUpdates
          api={api}
          scopeKey={scopeKey}
          instance={instance}
          files={updates.choice.files}
          generation={`${generation}:${resourceGeneration}`}
          native={native}
          disabled={updateBlocked}
          onTaskStart={onTaskStart}
          onClose={updates.close}
        />
      )}
      {detail && (
        <div
          className="ce-local-detail-backdrop"
          onClick={() => setDetail(null)}
        >
          <section
            className="ce-card ce-local-detail"
            role="dialog"
            aria-modal="true"
            aria-labelledby="ce-local-detail-title"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="ce-local-detail-heading">
              <h2 id="ce-local-detail-title">{t("local.details")}</h2>
              <button
                autoFocus
                aria-label={t("local.closeDetails")}
                onClick={() => setDetail(null)}
              >
                <X size={18} />
              </button>
            </div>
            <strong>{detail.name}</strong>
            {detail.version && (
              <p>{t("local.version", { version: detail.version })}</p>
            )}
            {detail.description && <p>{detail.description}</p>}
            <p>{t("local.file", { name: detail.file_name || detail.name })}</p>
            <p>
              {t("local.state", {
                state: t(detail.enabled ? "ui.enabled" : "ui.disabled"),
              })}
            </p>
            <p className="ce-local-detail-path">{detail.path}</p>
            <button
              className="ce-button primary"
              onClick={() => onOpen(section)}
            >
              {t("local.openFolder")}
            </button>
          </section>
        </div>
      )}
    </div>
  );
}
