import { t, formatNumber, formatRelativeDate, type MessageKey } from "./i18n";
import { useContext, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowUpToLine,
  Box,
  ChevronDown,
  Copy,
  Download,
  Globe,
  Heart,
  Languages,
  ListFilter,
  LoaderCircle,
  RefreshCw,
} from "lucide-react";
import { Collapse } from "./Collapse";
import type { Api, Instance } from "./types";
import type { PorterIntake } from "./experimentalPorterTypes";
import { ResourceInstall } from "./ResourceInstall";
import { ResourceSave } from "./ResourceSave";
import { InstanceImport } from "./InstanceImport";
import { ResourceFavorite } from "./LauncherFavorites";
import { LauncherNavigationContext } from "./useLauncherPreferences";
import type { ResourceInstallRequest } from "./resourceInstallPlan";
import {
  communityAliases,
  communityIcon,
  communitySource,
  mcmodResourceUrl,
  type CommunitySourceAlias,
} from "./communityResources";
import "./resource-details.css";
import "./community-resources.css";

export type ResourceSummary = {
  project_id: string;
  title: string;
  description: string;
  icon_url: string | null;
  categories: string[];
  display_categories: string[];
  versions: string[];
  downloads?: number;
  date_modified?: string;
  project_type?: string;
  source?: string;
  url?: string;
  identity_key?: string;
  sources?: CommunitySourceAlias[];
  attribution?: string;
  file_name?: string;
  local_version?: string;
  local_path?: string;
  enabled?: boolean;
  local_minecraft_version?: string;
  local_loader?: string;
};

type Project = Omit<ResourceSummary, "versions"> & {
  game_versions: string[];
  loaders: string[];
  url: string;
};
type DependencyReference = {
  project_id: string | null;
  version_id: string | null;
  file_name: string | null;
  dependency_type: string;
};
type Version = {
  id: string;
  project_id: string;
  name: string;
  version_number: string;
  game_versions: string[];
  loaders: string[];
  date_published: string;
  downloads: number;
  version_type: string;
  files: { filename: string; size: number; primary: boolean }[];
  dependencies: DependencyReference[];
};
type Details = {
  project: Project;
  versions: Version[];
  versions_truncated: boolean;
  sources?: CommunitySourceAlias[];
  identity_key?: string;
  attribution?: string;
};
type Dependencies = {
  version_id: string;
  dependencies: (DependencyReference & { project: Project | null })[];
  truncated: boolean;
};
type DependencyState = {
  data?: Dependencies;
  loading?: boolean;
  error?: string;
};
type VersionGroup = {
  id: string;
  title: string;
  game: string;
  loader: string;
  versions: Version[];
};

const loaderNames: Record<string, string> = {
  fabric: "Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
  quilt: "Quilt",
  liteloader: "LiteLoader",
  iris: "Iris",
  optifine: "OptiFine",
  minecraft: "Minecraft",
};
const categoryNames: Record<string, MessageKey> = {
  library: "category.library",
  technology: "category.technology",
  adventure: "category.adventure",
  optimization: "category.optimization",
  utility: "category.utility",
  decoration: "category.decoration",
  equipment: "category.equipmentTools",
  magic: "category.magic",
  storage: "category.warehouse",
  transportation: "category.transportation",
  worldgen: "category.worldgen",
  multiplayer: "category.multiplayer",
  lightweight: "category.lightweight",
  kitchen_sink: "category.kitchenSink",
  "game-mechanics": "category.mechanics",
  management: "nav.manage",
  cursed: "category.cursed",
  food: "category.food",
  mobs: "category.mobs",
  social: "category.social",
  visual: "category.visual",
  combat: "category.combat",
};
const loaderName = (value: string) => loaderNames[value.toLowerCase()] ?? value;
const isLoader = (value: string) =>
  Object.hasOwn(loaderNames, value.toLowerCase());
const count = (value?: number) =>
  value === undefined || !Number.isFinite(value)
    ? "—"
    : formatNumber(value, { notation: "compact", maximumFractionDigits: 1 });
const relativeDate = (value?: string) =>
  value ? formatRelativeDate(value) : "—";
const compareVersions = (a: string, b: string) =>
  b.localeCompare(a, "en", { numeric: true });
const stableGame = (value: string) => /^\d+(?:\.\d+)+$/.test(value);
const gameFamily = (value: string) =>
  value
    .match(/^(\d+(?:\.\d+)+)/)?.[1]
    .split(".")
    .slice(0, 2)
    .join(".") || "快照版";
// Only the locally synthesized fallback is translated. Remote version names
// and declared game versions are passed through unchanged.
const displayGroupTitle = (version: Version, title: string) =>
  version.game_versions.length
    ? title
    : title.replace("未标明游戏版本", t("resource.gameUnspecified"));
const unique = (values: string[]) => [...new Set(values)];
const fileSize = (value: number) =>
  value < 1_048_576
    ? `${formatNumber(value / 1024, { maximumFractionDigits: 1 })} KB`
    : `${formatNumber(value / 1048576, { maximumFractionDigits: 1 })} MB`;

function versionGroups(
  versions: Version[],
  multipleLoaders: boolean,
  pack: boolean,
): VersionGroup[] {
  const groups = new Map<string, VersionGroup>();
  for (const version of versions) {
    const games = version.game_versions.length
      ? version.game_versions
      : ["未标明游戏版本"];
    const loaders =
      multipleLoaders && !pack
        ? version.loaders.length
          ? version.loaders
          : [""]
        : [""];
    for (const game of games) {
      for (const loader of loaders) {
        const id = `${loader}\u0000${game}`;
        const group = groups.get(id) ?? {
          id,
          game,
          loader,
          title: [loaderName(loader), game].filter(Boolean).join(" "),
          versions: [],
        };
        group.versions.push(version);
        groups.set(id, group);
      }
    }
  }
  return [...groups.values()].sort(
    (a, b) =>
      compareVersions(a.game, b.game) || a.loader.localeCompare(b.loader),
  );
}

function ProjectIcon({
  url,
  local = false,
}: {
  url: string | null | undefined;
  local?: boolean;
}) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [url]);
  let safe = Boolean(communityIcon(url));
  safe ||=
    local &&
    !!url &&
    url.length < 2 * 1024 * 1024 &&
    /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(url);
  return (
    <span className="rd-project-icon">
      {safe && !failed ? (
        <img src={url!} alt="" onError={() => setFailed(true)} />
      ) : (
        <Box size={34} strokeWidth={1.5} />
      )}
    </span>
  );
}

function ReleaseIcon({ type }: { type: string }) {
  const label = type === "beta" ? "B" : type === "alpha" ? "A" : "R";
  return (
    <span
      className={`rd-release-icon rd-release-${label.toLowerCase()}`}
      aria-label={
        type === "beta"
          ? t("resource.beta")
          : type === "alpha"
            ? t("resource.alpha")
            : t("download.release")
      }
    >
      {label}
    </span>
  );
}

export function ResourceDetails({
  api,
  resource,
  onNotify,
  scopeKey,
  selectedInstance,
  native,
  disabled,
  saveDisabled = disabled,
  saveStartDisabled = false,
  saveStartDisabledReason,
  onTaskStart,
  onResourceDetails,
  onPorter,
  occupiedNames = [],
}: {
  api: Api;
  resource: ResourceSummary;
  onNotify: (message: string) => void;
  scopeKey: string;
  selectedInstance: Instance | null;
  native: boolean;
  disabled: boolean;
  saveDisabled?: boolean;
  saveStartDisabled?: boolean;
  saveStartDisabledReason?: string;
  onTaskStart: (id: string) => void;
  occupiedNames?: string[];
  onResourceDetails: (resource: ResourceSummary) => void;
  onPorter?: (intake: Omit<PorterIntake, "id">) => void;
}) {
  const saveAvailable = useContext(
    LauncherNavigationContext,
  ).standaloneSaveAvailable;
  const [detailResult, setDetailResult] = useState<{
    owner: object;
    data: Details;
  } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const source =
    communitySource(resource.source ?? "Modrinth") ??
    resource.source ??
    "Modrinth";
  const detailContextKey = JSON.stringify([
    scopeKey,
    source,
    resource.project_id,
    resource.project_type,
    retry,
  ]);
  // Source IDs can collide, and the same project can recur after changing root.
  // The owner retires data and stored handlers during render, before effect
  // cleanup. Selections and asynchronous replies must carry this exact owner.
  const detailOwner = useRef({ api, key: detailContextKey });
  if (
    detailOwner.current.api !== api ||
    detailOwner.current.key !== detailContextKey
  )
    detailOwner.current = { api, key: detailContextKey };
  const renderedDetailOwner = detailOwner.current;
  const details =
    detailResult?.owner === renderedDetailOwner ? detailResult.data : null;
  const [dependencyRetry, setDependencyRetry] = useState(0);
  // These sentinels identify filters, not display text. Translate at render so
  // locale changes preserve the selected filter and memoized version groups.
  const [gameFilter, setGameFilter] = useState("全部");
  const [loaderFilter, setLoaderFilter] = useState("全部");
  const [expanded, setExpanded] = useState<string[]>([]);
  const [selection, setSelected] = useState<{
    owner: object;
    version: Version;
    title: string;
  } | null>(null);
  const selected = selection?.owner === renderedDetailOwner ? selection : null;
  const [selectedOpen, setSelectedOpen] = useState(true);
  const [fileChoice, setFileChoice] = useState<{
    version: string;
    filename: string;
  } | null>(null);
  const [installChoice, setInstallChoice] = useState<{
    context: object;
    request: ResourceInstallRequest;
  } | null>(null);
  const [saveChoice, setSaveChoice] = useState<{
    context: object;
    request: ResourceInstallRequest;
  } | null>(null);
  const [dependencies, setDependencies] = useState<
    Record<string, DependencyState>
  >({});
  const [dependencyOpen, setDependencyOpen] = useState<Record<string, boolean>>(
    {},
  );
  const [packSelection, setPackChoice] = useState<{
    owner: object;
    version: Version;
  } | null>(null);
  const packChoice =
    packSelection?.owner === renderedDetailOwner ? packSelection.version : null;
  const [instanceName, setInstanceName] = useState("");
  const requestIdentity = useRef("");
  const requestGeneration = useRef(0);
  const dependencyRequests = useRef(new Set<string>());
  const selectedCard = useRef<HTMLElement>(null);
  const installButton = useRef<HTMLButtonElement>(null);
  const local = source.toLowerCase() === "local";
  const modrinth =
    source.toLowerCase() === "modrinth" && Boolean(resource.project_id);
  const curseforge = source === "CurseForge" && Boolean(resource.project_id);
  const encyclopedia = source === "MC百科";
  const downloadable = modrinth || curseforge;
  const pack =
    (details?.project.project_type ?? resource.project_type) === "modpack";
  const installPack = pack && modrinth;
  const installable =
    modrinth &&
    ["mod", "resourcepack", "shader"].includes(
      details?.project.project_type ?? resource.project_type ?? "",
    );
  const selectedFile =
    selected?.version.files.find(
      (file) =>
        fileChoice?.version === selected.version.id &&
        fileChoice.filename === file.filename,
    ) ??
    selected?.version.files.find((file) => file.primary) ??
    selected?.version.files[0];
  const contextKey = JSON.stringify([
    scopeKey,
    source,
    resource.project_id,
    retry,
    selectedInstance?.id,
    selectedInstance?.minecraft_version,
    selectedInstance?.loader,
    selected?.version.id,
    selectedFile?.filename,
  ]);
  // Equal IDs can reappear after browsing another root. The owner object also
  // invalidates saved actions and an open confirmation when that scope changes.
  const installationContext = useRef({ api, contextKey });
  if (
    installationContext.current.api !== api ||
    installationContext.current.contextKey !== contextKey
  )
    installationContext.current = { api, contextKey };
  const renderedInstallationContext = installationContext.current;
  const admission = useRef({ native, disabled, saveDisabled });
  admission.current = { native, disabled, saveDisabled };
  const project = details?.project;
  const title = project?.title ?? resource.title;
  const loaders = project?.loaders ?? resource.categories.filter(isLoader);
  const games = project?.game_versions ?? resource.versions;
  const releases = games.filter(stableGame).sort(compareVersions);
  const supportedGames =
    releases.length > 1 ? `${releases.at(-1)}+` : releases[0] || games[0] || "";
  const categories = (
    project?.display_categories ?? resource.display_categories
  ).filter((item) => !isLoader(item));
  const multipleLoaders = loaders.length > 1;
  const groups = useMemo(
    () => versionGroups(details?.versions ?? [], multipleLoaders, pack),
    [details, multipleLoaders, pack],
  );
  const filters = unique(
    games.map((game) => (pack ? game : gameFamily(game))),
  ).sort((a, b) =>
    a === "快照版" ? 1 : b === "快照版" ? -1 : compareVersions(a, b),
  );
  const shownGroups = groups
    .filter(
      (group) =>
        ((gameFilter === "全部" && (pack || stableGame(group.game))) ||
          (gameFilter === "快照版" && !stableGame(group.game)) ||
          (gameFilter !== "全部" &&
            gameFilter !== "快照版" &&
            (pack
              ? group.game === gameFilter
              : gameFamily(group.game) === gameFilter))) &&
        (loaderFilter === "全部" ||
          group.loader === loaderFilter ||
          (!group.loader &&
            group.versions.some((version) =>
              version.loaders.includes(loaderFilter),
            ))),
    )
    .map((group) => ({
      ...group,
      versions: group.versions.filter(
        (version) =>
          loaderFilter === "全部" || version.loaders.includes(loaderFilter),
      ),
    }));
  const providerUrl =
    project?.url ??
    resource.url ??
    (modrinth
      ? `https://modrinth.com/${resource.project_type === "modpack" ? "modpack" : resource.project_type === "resourcepack" ? "resourcepack" : resource.project_type === "shader" ? "shader" : "mod"}/${encodeURIComponent(resource.project_id)}`
      : "");
  // Encyclopedia details re-read the complete related-links section and can
  // retire a search snapshot after detecting an identity conflict. Provider
  // details only describe their own primary project, so those retain the
  // previously verified search links until encyclopedia evidence replaces it.
  const aliases =
    encyclopedia && details
      ? communityAliases({
          ...resource,
          source,
          url: providerUrl,
          sources: details.sources ?? project?.sources ?? [],
        })
      : communityAliases(
          { ...resource, source, url: providerUrl },
          details?.sources,
          project?.sources,
        );
  const encyclopediaUrl = mcmodResourceUrl(title, aliases);
  const favoriteProject = aliases.find(
    (alias) => alias.source === "Modrinth" && alias.project_id,
  )?.project_id;
  function changeSource(alias: CommunitySourceAlias) {
    if (detailOwner.current !== renderedDetailOwner) return;
    if (!alias.project_id) {
      void openLink(alias.url);
      return;
    }
    onResourceDetails({
      ...resource,
      project_id: alias.project_id,
      source: alias.source,
      url: alias.url,
      title,
      project_type: project?.project_type ?? resource.project_type,
      identity_key:
        details?.identity_key ?? project?.identity_key ?? resource.identity_key,
      sources: aliases,
    });
  }

  useEffect(() => {
    const identity = `${source}:${resource.project_id}:${++requestGeneration.current}`;
    requestIdentity.current = identity;
    dependencyRequests.current = new Set();
    setDetailResult(null);
    setError("");
    setExpanded([]);
    setDependencies({});
    setDependencyOpen({});
    setSelected(null);
    setFileChoice(null);
    setInstallChoice(null);
    setSaveChoice(null);
    setPackChoice(null);
    setGameFilter("全部");
    setLoaderFilter("全部");
    if (local || !communitySource(source) || !resource.project_id) {
      setLoading(false);
      return;
    }
    let live = true;
    setLoading(true);
    api<Details>("community_details", {
      source,
      projectId: resource.project_id,
      projectType: resource.project_type,
    })
      .then((next) => {
        if (!live || detailOwner.current !== renderedDetailOwner) return;
        setDetailResult({ owner: renderedDetailOwner, data: next });
        const nextPack = next.project.project_type === "modpack";
        const groupList = versionGroups(
          next.versions,
          next.project.loaders.length > 1,
          next.project.project_type === "modpack",
        );
        if (
          next.project.loaders.length <= 1 ||
          next.project.project_type === "modpack"
        )
          setExpanded(
            (
              nextPack
                ? groupList[0]
                : groupList.find((group) => stableGame(group.game))
            )
              ? [
                  (nextPack
                    ? groupList[0]
                    : groupList.find((group) => stableGame(group.game)))!.id,
                ]
              : [],
          );
      })
      .catch((e) => {
        if (live && detailOwner.current === renderedDetailOwner)
          setError(String(e));
      })
      .finally(() => {
        if (live && detailOwner.current === renderedDetailOwner)
          setLoading(false);
      });
    return () => {
      live = false;
      if (requestIdentity.current === identity) requestIdentity.current = "";
    };
  }, [renderedDetailOwner]);

  const visibleDependencyVersions = unique(
    [
      ...shownGroups
        .filter((group) => expanded.includes(group.id))
        .map((group) => group.versions[0]?.id ?? ""),
      selected?.version.id ?? "",
    ].filter(Boolean),
  );
  const dependencyKey = visibleDependencyVersions.join(",");
  useEffect(() => {
    if (!modrinth || !details) return;
    const identity = requestIdentity.current;
    for (const versionId of dependencyKey.split(",").filter(Boolean)) {
      if (dependencyRequests.current.has(versionId)) continue;
      dependencyRequests.current.add(versionId);
      const version = details.versions.find((entry) => entry.id === versionId);
      if (
        !version?.dependencies.some((dependency) =>
          ["required", "optional"].includes(dependency.dependency_type),
        )
      ) {
        setDependencies((state) => ({
          ...state,
          [versionId]: {
            data: { version_id: versionId, dependencies: [], truncated: false },
          },
        }));
        continue;
      }
      setDependencies((state) => ({
        ...state,
        [versionId]: { loading: true },
      }));
      api<Dependencies>("resource_dependencies", { versionId })
        .then((data) => {
          if (
            requestIdentity.current === identity &&
            detailOwner.current === renderedDetailOwner
          )
            setDependencies((state) => ({ ...state, [versionId]: { data } }));
        })
        .catch((e) => {
          if (
            requestIdentity.current === identity &&
            detailOwner.current === renderedDetailOwner
          )
            setDependencies((state) => ({
              ...state,
              [versionId]: { error: String(e) },
            }));
        });
    }
  }, [
    api,
    dependencyKey,
    details,
    modrinth,
    dependencyRetry,
    renderedDetailOwner,
  ]);

  async function openLink(url: string) {
    if (detailOwner.current !== renderedDetailOwner) return;
    if (!url) {
      onNotify(t("resource.providerUnavailable", { source }));
      return;
    }
    try {
      await api("resource_open_link", { url });
    } catch (e) {
      if (detailOwner.current === renderedDetailOwner) onNotify(String(e));
    }
  }
  async function copy(value: string, label: string) {
    if (detailOwner.current !== renderedDetailOwner) return;
    if (!value) {
      onNotify(t("resource.sourceUnmatched"));
      return;
    }
    try {
      await navigator.clipboard.writeText(value);
      if (detailOwner.current === renderedDetailOwner)
        onNotify(t("resource.copied", { label }));
    } catch {
      if (detailOwner.current === renderedDetailOwner)
        onNotify(t("resource.clipboardError"));
    }
  }
  function chooseVersion(version: Version, groupTitle: string) {
    if (detailOwner.current !== renderedDetailOwner) return;
    setSelected({ owner: renderedDetailOwner, version, title: groupTitle });
    if (selected?.version.id !== version.id) setFileChoice(null);
    setInstallChoice(null);
    setSaveChoice(null);
    setSelectedOpen(true);
    if (pack && modrinth && native && !disabled && scopeKey) {
      setInstanceName(
        title
          .replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ")
          .trim()
          .slice(0, 100),
      );
      setPackChoice({ owner: renderedDetailOwner, version });
    } else {
      window.requestAnimationFrame(() => {
        if (detailOwner.current !== renderedDetailOwner) return;
        selectedCard.current?.scrollIntoView({
          block: "nearest",
          behavior: "smooth",
        });
      });
    }
  }
  function versionRow(version: Version, groupTitle: string) {
    const dependencyCount = modrinth
      ? version.dependencies.filter(
          (dependency) => dependency.dependency_type === "required",
        ).length
      : 0;
    return (
      <button
        className={`rd-version-row ${selected?.version.id === version.id ? "is-selected" : ""}`}
        key={version.id}
        onClick={() => chooseVersion(version, groupTitle)}
      >
        <ReleaseIcon type={version.version_type} />
        <span className="rd-version-copy">
          <span className="rd-version-title">
            {version.name ||
              version.files.find((file) => file.primary)?.filename ||
              version.version_number}
          </span>
          <span className="rd-version-meta">
            {version.version_number}
            {dependencyCount > 0 &&
              t("resource.dependencyCount", {
                count: formatNumber(dependencyCount),
              })}{" "}
            |{" "}
            {t("resource.downloadMeta", {
              count: count(version.downloads),
              date: relativeDate(version.date_published),
            })}
            {version.version_type === "beta"
              ? t("resource.betaSuffix")
              : version.version_type === "alpha"
                ? t("resource.alphaSuffix")
                : ""}
          </span>
        </span>
        <Download size={17} className="rd-row-download" aria-hidden="true" />
      </button>
    );
  }
  function dependencySections(version: Version, context: string) {
    if (!modrinth) return null;
    const state = dependencies[version.id];
    const references =
      state?.data?.dependencies ??
      version.dependencies.map((reference) => ({
        ...reference,
        project: null,
      }));
    return (
      <>
        {["required", "optional"].map((kind) => {
          const entries = references.filter(
            (dependency) => dependency.dependency_type === kind,
          );
          if (!entries.length) return null;
          const key = `${context}:${kind}`;
          const open = dependencyOpen[key] !== false;
          return (
            <section className="rd-dependencies" key={kind}>
              <button
                className="rd-dependency-toggle"
                onClick={() =>
                  setDependencyOpen((value) => ({ ...value, [key]: !open }))
                }
                aria-expanded={open}
              >
                <strong>
                  {kind === "required"
                    ? t("resource.required")
                    : t("resource.optional")}
                  （{formatNumber(entries.length)}）
                </strong>
                <ChevronDown size={16} className={open ? "is-open" : ""} />
              </button>
              <Collapse open={open}>
                {entries.map((dependency, index) => (
                  <button
                    type="button"
                    className="rd-dependency-row"
                    key={`${dependency.project_id ?? dependency.version_id ?? dependency.file_name}:${index}`}
                    disabled={!dependency.project?.project_id}
                    onClick={() => {
                      const project = dependency.project;
                      if (
                        !project?.project_id ||
                        detailOwner.current !== renderedDetailOwner
                      )
                        return;
                      // Browsing uses the resolved provider identity. It is
                      // independent of required/optional installation choices.
                      onResourceDetails({
                        project_id: project.project_id,
                        title: project.title,
                        description: project.description,
                        icon_url: project.icon_url,
                        categories: project.categories,
                        display_categories: project.display_categories,
                        versions: project.game_versions,
                        downloads: project.downloads,
                        date_modified: project.date_modified,
                        project_type: project.project_type,
                        source: "Modrinth",
                      });
                    }}
                    aria-label={t("ui.viewDetails", {
                      name:
                        dependency.project?.title ??
                        dependency.file_name ??
                        dependency.project_id ??
                        dependency.version_id ??
                        t("resource.unnamedDependency"),
                    })}
                  >
                    <ProjectIcon url={dependency.project?.icon_url} />
                    <span className="rd-dependency-copy">
                      <span className="rd-dependency-name">
                        {dependency.project?.title ??
                          dependency.file_name ??
                          dependency.project_id ??
                          dependency.version_id ??
                          t("resource.unnamedDependency")}
                      </span>
                      {dependency.project ? (
                        <>
                          <span className="rd-description">
                            {dependency.project.display_categories
                              .filter((category) => !isLoader(category))
                              .map((category) => (
                                <span className="rd-category" key={category}>
                                  {categoryNames[category]
                                    ? t(categoryNames[category])
                                    : category}
                                </span>
                              ))}
                            {dependency.project.description}
                          </span>
                          <span className="rd-dependency-meta">
                            <span>
                              <Download size={12} />
                              {count(dependency.project.downloads)}
                            </span>
                            <span>
                              <ArrowUpToLine size={12} />
                              {relativeDate(dependency.project.date_modified)}
                            </span>
                            <span>
                              <Globe size={12} />
                              Modrinth
                            </span>
                          </span>
                        </>
                      ) : (
                        <span className="rd-description">
                          {state?.loading
                            ? t("resource.readingDependency")
                            : t("resource.dependencyUnavailable")}
                        </span>
                      )}
                    </span>
                  </button>
                ))}
              </Collapse>
            </section>
          );
        })}
        {state?.error && (
          <div className="rd-inline-status" role="status">
            {t("resource.dependencyError", { error: state.error || "" })}
            <button
              className="ce-text-button"
              onClick={() => {
                dependencyRequests.current.delete(version.id);
                setDependencyRetry((value) => value + 1);
              }}
            >
              {t("ui.retry")}
            </button>
          </div>
        )}
        {state?.data?.truncated && (
          <div className="rd-inline-status">
            {t("resource.dependencyTruncated")}
          </div>
        )}
      </>
    );
  }

  return (
    <div className="rd-page">
      <section className="ce-card rd-summary">
        <div className="rd-summary-main">
          <ProjectIcon
            url={project?.icon_url ?? resource.icon_url}
            local={local}
          />
          <div className="rd-summary-copy">
            <h1>{title}</h1>
            <div className="rd-description">
              {categories.map((category) => (
                <span className="rd-category" key={category}>
                  {categoryNames[category]
                    ? t(categoryNames[category])
                    : category}
                </span>
              ))}
              {project?.description ?? resource.description}
            </div>
            <div className="rd-summary-meta">
              <span title={games.join(", ")}>
                <ListFilter size={12} />
                {local
                  ? [resource.local_loader, resource.local_minecraft_version]
                      .filter(Boolean)
                      .join(" ") || t("resource.localMod")
                  : `${loaders.map(loaderName).join(" / ")} ${supportedGames}`}
              </span>
              <span>
                <Download size={12} />
                {count(project?.downloads ?? resource.downloads)}
              </span>
              <span title={project?.date_modified ?? resource.date_modified}>
                <ArrowUpToLine size={12} />
                {relativeDate(project?.date_modified ?? resource.date_modified)}
              </span>
              <span>
                <Globe size={12} />
                {local ? t("resource.localFile") : source}
              </span>
            </div>
          </div>
        </div>
        <div className="rd-summary-actions">
          <button onClick={() => openLink(providerUrl)}>
            <Globe size={15} />
            {source === "local" ? t("resource.sourcePage") : source}
          </button>
          <button onClick={() => openLink(encyclopediaUrl)}>
            <Globe size={15} />
            {t("resource.mcmod")}
          </button>
          <button onClick={() => copy(title, t("ui.name"))}>
            <Copy size={15} />
            {t("resource.copyName")}
          </button>
          <button onClick={() => copy(providerUrl, t("ui.link"))}>
            <Copy size={15} />
            {t("resource.copyLink")}
          </button>
          <button onClick={() => onNotify(t("resource.translateUnavailable"))}>
            <Languages size={15} />
            {t("resource.translate")}
          </button>
          {favoriteProject && (
            <ResourceFavorite
              projectId={favoriteProject}
              supported={["mod", "resourcepack", "shader", "modpack"].includes(
                project?.project_type || resource.project_type || "",
              )}
              contextKey={contextKey}
            />
          )}
        </div>
        {aliases.length > 1 && (
          <div className="rd-source-links">
            {aliases.map((alias) => {
              const current =
                alias.source === source &&
                alias.project_id === resource.project_id;
              return (
                <button
                  key={`${alias.source}:${alias.project_id ?? alias.url}`}
                  className={`ce-community-badge ${current ? "is-current" : ""}`}
                  disabled={current}
                  aria-current={current ? "page" : undefined}
                  title={t(
                    alias.project_id
                      ? "community.sourceSwitch"
                      : "community.sourceOpen",
                    { source: alias.source },
                  )}
                  onClick={() => changeSource(alias)}
                >
                  <Globe size={12} />
                  {alias.source}
                </button>
              );
            })}
          </div>
        )}
        {(details?.attribution ??
          project?.attribution ??
          resource.attribution) && (
          <p className="rd-inline-status">
            {details?.attribution ??
              project?.attribution ??
              resource.attribution}
          </p>
        )}
      </section>
      {local ? (
        <>
          <section className="ce-card rd-version-group">
            <button
              className="rd-group-toggle"
              onClick={() => setSelectedOpen((value) => !value)}
              aria-expanded={selectedOpen}
            >
              <strong>
                {t("resource.selectedVersion", {
                  version:
                    [resource.local_loader, resource.local_minecraft_version]
                      .filter(Boolean)
                      .join(" ") ||
                    resource.local_version ||
                    t("resource.localFile"),
                })}
              </strong>
              <ChevronDown
                size={16}
                className={selectedOpen ? "is-open" : ""}
              />
            </button>
            <Collapse open={selectedOpen}>
              <div className="rd-group-body">
                <div className="rd-version-row">
                  <ReleaseIcon type="release" />
                  <div className="rd-version-copy">
                    <span className="rd-version-title">
                      {resource.file_name ?? title}
                    </span>
                    <span className="rd-version-meta">
                      {resource.local_version ||
                        t("resource.modVersionUnspecified")}
                      {resource.enabled !== undefined &&
                        `  |  ${resource.enabled ? t("ui.enabled") : t("ui.disabled")}`}
                    </span>
                  </div>
                </div>
                {resource.local_path && (
                  <div className="rd-local-path">
                    {t("resource.fileLocation", {
                      path: resource.local_path || "",
                    })}
                  </div>
                )}
              </div>
            </Collapse>
          </section>
          <section className="ce-card rd-state">
            <p>{t("resource.noCatalog")}</p>
            <small>{t("resource.localOnlyHelp")}</small>
          </section>
        </>
      ) : (
        <>
          {!encyclopedia && (
            <section
              className={`ce-card rd-filters ${pack ? "rd-pack-filters" : ""}`}
            >
              <div className="rd-filter-row">
                {!pack && <span>{t("resource.instanceFilter")}</span>}
                {["全部", ...filters].map((value) => (
                  <button
                    key={value}
                    className={gameFilter === value ? "is-active" : ""}
                    onClick={() => setGameFilter(value)}
                  >
                    {value === "全部"
                      ? t("ui.all")
                      : value === "快照版"
                        ? t("download.snapshot")
                        : value}
                  </button>
                ))}
              </div>
              {!pack && (
                <div className="rd-filter-row">
                  <span>{t("resource.loaderFilter")}</span>
                  {["全部", ...unique(loaders)].map((value) => (
                    <button
                      key={value}
                      className={loaderFilter === value ? "is-active" : ""}
                      onClick={() => setLoaderFilter(value)}
                    >
                      {value === "全部" ? t("ui.all") : loaderName(value)}
                    </button>
                  ))}
                </div>
              )}
            </section>
          )}
          {loading && (
            <section className="ce-card rd-state" role="status">
              <LoaderCircle size={18} className="spin" />
              <p>{t("resource.reading")}</p>
            </section>
          )}
          {error && (
            <section className="ce-card rd-state" role="status">
              <p>{error}</p>
              <button
                className="ce-button"
                onClick={() => setRetry((value) => value + 1)}
              >
                <RefreshCw size={14} />
                {t("ui.reread")}
              </button>
            </section>
          )}
          {!communitySource(source) && (
            <section className="ce-card rd-state">
              <p>{t("resource.providerUnavailable", { source })}</p>
              <small>{t("resource.providerHelp")}</small>
            </section>
          )}
          {encyclopedia && !loading && !error && (
            <section className="ce-card rd-state">
              <p>{t("community.mcmodReference")}</p>
            </section>
          )}
          {curseforge && (
            <p className="rd-inline-status">
              {t("community.curseforgeDownloadOnly")}
            </p>
          )}
          {selected && (
            <section
              className="ce-card rd-version-group rd-selected"
              ref={selectedCard}
            >
              <button
                className="rd-group-toggle"
                onClick={() => setSelectedOpen((value) => !value)}
                aria-expanded={selectedOpen}
              >
                <strong>
                  {t("resource.selectedVersion", {
                    version: displayGroupTitle(
                      selected.version,
                      selected.title,
                    ),
                  })}
                </strong>
                <ChevronDown
                  size={16}
                  className={selectedOpen ? "is-open" : ""}
                />
              </button>
              <Collapse open={selectedOpen}>
                <div className="rd-group-body">
                  {dependencySections(selected.version, "selected")}
                  <div className="rd-version-list-label">
                    {t("resource.versionList")}
                  </div>
                  {versionRow(selected.version, selected.title)}
                  <div className="rd-selected-files">
                    {selected.version.files.map((file, index) =>
                      selected.version.files.length > 1 ? (
                        <button
                          key={`${file.filename}:${index}`}
                          className="ce-text-button rd-selected-file"
                          type="button"
                          aria-pressed={
                            selectedFile?.filename === file.filename
                          }
                          onClick={() => {
                            if (
                              installationContext.current !==
                              renderedInstallationContext
                            )
                              return;
                            setFileChoice({
                              version: selected.version.id,
                              filename: file.filename,
                            });
                            setInstallChoice(null);
                            setSaveChoice(null);
                          }}
                        >
                          {file.filename}（{fileSize(file.size)}）
                        </button>
                      ) : (
                        <span key={`${file.filename}:${index}`}>
                          {file.filename}（{fileSize(file.size)}）
                        </span>
                      ),
                    )}
                  </div>
                  <div className="rd-selected-actions">
                    {onPorter &&
                      modrinth &&
                      (project?.project_type || resource.project_type) ===
                        "mod" && (
                        <button
                          className="ce-button"
                          disabled={!native}
                          onClick={() => {
                            if (detailOwner.current !== renderedDetailOwner)
                              return;
                            onPorter({
                              title: project?.title || resource.title,
                              url: `https://modrinth.com/mod/${selected.version.project_id}/version/${selected.version.id}`,
                            });
                          }}
                        >
                          {t("experimental.porterCreateFromResource")}
                        </button>
                      )}
                    <button
                      className="ce-button primary"
                      disabled={
                        installPack
                          ? !native ||
                            disabled ||
                            !modrinth ||
                            !scopeKey ||
                            !selectedFile?.filename.endsWith(".mrpack")
                          : !native ||
                            !saveAvailable ||
                            saveDisabled ||
                            !downloadable ||
                            !selectedFile
                      }
                      title={
                        !installPack && (!native || !saveAvailable)
                          ? t("resource.downloadUnavailable")
                          : undefined
                      }
                      onClick={() => {
                        if (installPack) {
                          chooseVersion(selected.version, selected.title);
                          return;
                        }
                        if (
                          installationContext.current !==
                            renderedInstallationContext ||
                          !admission.current.native ||
                          admission.current.saveDisabled ||
                          !saveAvailable ||
                          !downloadable ||
                          !selectedFile
                        )
                          return;
                        setInstallChoice(null);
                        setSaveChoice({
                          context: renderedInstallationContext,
                          request: {
                            ...(curseforge
                              ? { source: "CurseForge" as const }
                              : {}),
                            project_id: selected.version.project_id,
                            version_id: selected.version.id,
                            file_name: selectedFile.filename,
                          },
                        });
                      }}
                    >
                      <Download size={15} />
                      {installPack
                        ? t("resource.installPack")
                        : t("resource.downloadFile")}
                    </button>
                    {installable && (
                      <button
                        ref={installButton}
                        className="ce-button"
                        disabled={
                          !native ||
                          disabled ||
                          !scopeKey ||
                          !selectedInstance ||
                          !selectedFile
                        }
                        onClick={() => {
                          if (
                            installationContext.current !==
                              renderedInstallationContext ||
                            !admission.current.native ||
                            admission.current.disabled ||
                            !scopeKey ||
                            !selectedInstance ||
                            !selectedFile
                          )
                            return;
                          setSaveChoice(null);
                          setInstallChoice({
                            context: renderedInstallationContext,
                            request: {
                              project_id: selected.version.project_id,
                              version_id: selected.version.id,
                              file_name: selectedFile.filename,
                            },
                          });
                        }}
                      >
                        <Download size={15} />
                        {t("resource.install")}
                      </button>
                    )}
                  </div>
                </div>
              </Collapse>
            </section>
          )}
          {shownGroups.map((group) => {
            const open = expanded.includes(group.id);
            const first = group.versions[0];
            const hasDependencies =
              modrinth &&
              first?.dependencies.some((dependency) =>
                ["required", "optional"].includes(dependency.dependency_type),
              );
            return (
              <section className="ce-card rd-version-group" key={group.id}>
                <button
                  className="rd-group-toggle"
                  onClick={() =>
                    setExpanded((current) =>
                      open
                        ? current.filter((id) => id !== group.id)
                        : [...current, group.id],
                    )
                  }
                  aria-expanded={open}
                >
                  <strong>
                    {group.game === "未标明游戏版本"
                      ? [
                          loaderName(group.loader),
                          t("resource.gameUnspecified"),
                        ]
                          .filter(Boolean)
                          .join(" ")
                      : group.title}
                  </strong>
                  <ChevronDown size={16} className={open ? "is-open" : ""} />
                </button>
                <Collapse open={open}>
                  <div className="rd-group-body">
                    {first && dependencySections(first, group.id)}
                    {hasDependencies && (
                      <div className="rd-version-list-label">
                        {t("resource.versionList")}
                      </div>
                    )}
                    {group.versions.map((version) =>
                      versionRow(version, group.title),
                    )}
                  </div>
                </Collapse>
              </section>
            );
          })}
          {!encyclopedia &&
            details &&
            !loading &&
            !error &&
            !shownGroups.length && (
              <section className="ce-card rd-state">
                <p>{t("resource.noFilteredVersions")}</p>
              </section>
            )}
          {details?.versions_truncated && (
            <p className="rd-inline-status">
              {t("resource.versionsTruncated")}
            </p>
          )}
        </>
      )}
      {saveChoice?.context === renderedInstallationContext && (
        <ResourceSave
          api={api}
          request={saveChoice.request}
          contextKey={contextKey}
          native={native && saveAvailable}
          disabled={saveDisabled}
          startDisabled={saveStartDisabled}
          startDisabledReason={saveStartDisabledReason}
          onTaskStart={onTaskStart}
          onClose={() => setSaveChoice(null)}
        />
      )}
      {installChoice?.context === renderedInstallationContext && (
        <ResourceInstall
          api={api}
          scopeKey={scopeKey}
          selectedInstance={selectedInstance}
          request={installChoice.request}
          contextKey={contextKey}
          native={native}
          disabled={disabled}
          onTaskStart={onTaskStart}
          onClose={() => {
            setInstallChoice(null);
            setSaveChoice(null);
            installButton.current?.focus();
          }}
          onNotify={onNotify}
        />
      )}
      {modrinth && packChoice && selectedFile && (
        <InstanceImport
          key={contextKey}
          api={api}
          scopeKey={contextKey}
          native={native}
          disabled={disabled}
          occupiedNames={occupiedNames}
          official={{
            request: {
              project_id: packChoice.project_id,
              version_id: packChoice.id,
              file_name: selectedFile.filename,
            },
            name: instanceName,
          }}
          onTaskStart={onTaskStart}
          onClose={() => setPackChoice(null)}
          onNotify={onNotify}
        />
      )}
    </div>
  );
}
