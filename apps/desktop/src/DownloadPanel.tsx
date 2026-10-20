import { CeSelect } from "./CeSelect";
import {
  t,
  formatNumber,
  formatDate,
  formatRelativeDate,
  type MessageKey,
} from "./i18n";
import { useEffect, useRef, useState } from "react";
import {
  Box,
  Pickaxe,
  ChevronDown,
  Download,
  Globe,
  ListFilter,
  ArrowUpToLine,
  LoaderCircle,
  RefreshCw,
  Search,
  TriangleAlert,
  X,
} from "lucide-react";

import { Favorites, LoaderCatalog, installerPages } from "./LoaderCatalog";
import { Collapse } from "./Collapse";
import { InstallSelection, installNameError } from "./InstallSelection";
import type { InstallOptions } from "./InstallSelection";
import type { ResourceSummary } from "./ResourceDetails";
import type { ResourceBrowseRequest } from "./resourceBrowse";
import { communityAliases, communityIcon } from "./communityResources";
import { CommunityPages, type CommunityPage } from "./communityPagination";
import grassIcon from "./assets/game-icons/grass.svg";
import commandIcon from "./assets/game-icons/command.svg";
import "./community-loading.css";
import "./community-resources.css";

import { idleDownload, type DownloadStatus } from "./taskTypes";
export { idleDownload } from "./taskTypes";
export type {
  DownloadStatus,
  DownloadStep,
  DownloadTaskView,
} from "./taskTypes";
type VersionEntry = { id: string; kind: string; release_time: string };
const kindName = (kind: string) =>
  kind === "release"
    ? t("download.release")
    : kind === "snapshot"
      ? t("download.snapshot")
      : t("download.legacy");

export function DownloadPanel({
  api,
  native,
  rootId,
  rootAvailable = true,
  installed,
  gameBusy,
  onResourceDetails,
  onTaskStart,
  visible = true,
  section = "minecraft",
  compatibility,
}: {
  api: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  native: boolean;
  rootId: string | null;
  rootAvailable?: boolean;
  section?: string;
  compatibility?: ResourceBrowseRequest & { sequence: number };
  installed: { id: string }[];
  gameBusy: boolean;
  onResourceDetails: (resource: ResourceSummary) => void;
  onTaskStart: (id: string) => void;
  visible?: boolean;
}) {
  const [catalog, setCatalog] = useState<VersionEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<string[]>([]);
  const [choice, setChoice] = useState<VersionEntry | null>(null);
  const [working, setWorking] = useState(false);
  const starting = useRef(false);
  const mounted = useRef(true);
  const submissionEpoch = useRef(0);
  const catalogEpoch = useRef(0);
  const context = useRef({ rootId, section, visible, api, choice });
  if (
    context.current.rootId !== rootId ||
    context.current.section !== section ||
    context.current.visible !== visible ||
    context.current.api !== api ||
    context.current.choice !== choice
  )
    context.current = { rootId, section, visible, api, choice };
  const renderedContext = context.current;
  const callbacks = useRef({
    onTaskStart,
  });
  callbacks.current = {
    onTaskStart,
  };
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      submissionEpoch.current += 1;
      catalogEpoch.current += 1;
    };
  }, []);
  useEffect(() => {
    setChoice(null);
    setError("");
  }, [section, rootId]);
  async function loadCatalog(refresh = false) {
    const request = ++catalogEpoch.current;
    setLoading(true);
    setError("");
    try {
      const entries = await api<VersionEntry[]>("download_catalog", {
        refresh,
      });
      if (mounted.current && request === catalogEpoch.current)
        setCatalog(entries);
    } catch (e) {
      if (mounted.current && request === catalogEpoch.current)
        setError(String(e));
    } finally {
      if (mounted.current && request === catalogEpoch.current)
        setLoading(false);
    }
  }
  useEffect(() => {
    void loadCatalog();
    return () => {
      catalogEpoch.current += 1;
    };
  }, [api]);
  // Global task polling belongs to App. This panel only owns its submission
  // lifetime, so another queued/running job does not disable catalog choices.
  const busy = working;
  const ids = new Set(installed.map((item) => item.id));
  const aprilVersions = new Set([
    "26w14a",
    "25w14craftmine",
    "24w14potato",
    "23w13a_or_b",
    "22w13oneblockatatime",
    "20w14infinite",
    "3D Shareware v1.34",
    "1.RV-Pre1",
    "15w14a",
    "2.0",
  ]);
  const groups = [
    {
      id: "release",
      title: t("download.release"),
      entries: catalog.filter(
        (e) => e.kind === "release" && !aprilVersions.has(e.id),
      ),
    },
    {
      id: "snapshot",
      title: t("download.preview"),
      entries: catalog.filter(
        (e) => e.kind === "snapshot" && !aprilVersions.has(e.id),
      ),
    },
    {
      id: "old",
      title: t("download.ancient"),
      entries: catalog.filter(
        (e) =>
          !["release", "snapshot"].includes(e.kind) && !aprilVersions.has(e.id),
      ),
    },
    {
      id: "april",
      title: t("download.april"),
      entries: catalog.filter((e) => aprilVersions.has(e.id)),
    },
  ];
  const latest = [
    catalog.find((e) => e.kind === "release"),
    catalog.find((e) => e.kind === "snapshot" && !aprilVersions.has(e.id)),
  ].filter((e): e is VersionEntry => Boolean(e));
  const resourceLabels: Record<string, string> = {
    mods: "模组",
    modpacks: "整合包",
    datapacks: "数据包",
    resourcepacks: "资源包",
    shaders: "光影包",
    worlds: "世界",
    favorites: "收藏夹",
  };
  const community = resourceLabels[section];
  function versionRow(entry: VersionEntry, newest = false) {
    return (
      <button
        key={entry.id}
        className={`resource-row ce-version-row ${choice?.id === entry.id ? "chosen" : ""}`}
        disabled={busy || gameBusy || !rootAvailable}
        onClick={() => setChoice(entry)}
      >
        <span
          className={`resource-icon ce-minecraft-icon ${entry.kind === "release" ? "grass" : "command"}`}
        >
          <img
            src={entry.kind === "release" ? grassIcon : commandIcon}
            alt=""
          />
        </span>
        <span className="ce-resource-text">
          <strong>{entry.id}</strong>
          <small>
            {newest
              ? entry.kind === "release"
                ? t("download.latestRelease")
                : t("download.latestPreview")
              : kindName(entry.kind)}
            {t("download.published")}{" "}
            {formatDate(new Date(entry.release_time), {
              year: "numeric",
              month: "numeric",
              day: "numeric",
              hour: "2-digit",
              minute: "2-digit",
              hour12: false,
            })}
          </small>
        </span>
        {ids.has(entry.id) && <span className="tag">{t("ui.installed")}</span>}
      </button>
    );
  }
  async function start(options: InstallOptions) {
    if (
      starting.current ||
      context.current !== renderedContext ||
      !context.current.visible ||
      !choice ||
      gameBusy ||
      busy ||
      !native ||
      !rootAvailable ||
      !rootId ||
      installNameError(options.name, installed)
    )
      return;
    const source = context.current;
    const targetRootId = rootId;
    const targetVersion = choice.id;
    starting.current = true;
    const epoch = ++submissionEpoch.current;
    setWorking(true);
    setError("");
    try {
      const taskId = await api<string>("download_start", {
        id: targetVersion,
        rootId: targetRootId,
        name: options.name,
        components: options.components,
      });
      if (
        !mounted.current ||
        epoch !== submissionEpoch.current ||
        context.current.api !== source.api
      )
        return;
      if (source === context.current && source.visible) {
        setChoice(null);
        callbacks.current.onTaskStart(taskId);
      }
    } catch (e) {
      if (
        mounted.current &&
        epoch === submissionEpoch.current &&
        source === context.current
      )
        setError(String(e));
    } finally {
      if (epoch === submissionEpoch.current) {
        starting.current = false;
        if (mounted.current) setWorking(false);
      }
    }
  }
  return (
    <>
      {gameBusy && <div className="auth-notice">{t("download.gameBusy")}</div>}
      {error && (
        <div className="error-banner" role="alert">
          <TriangleAlert size={17} />
          <span>{error}</span>
          <button
            className="icon-button"
            onClick={() => setError("")}
            aria-label={t("ui.closeNotice")}
          >
            <X size={15} />
          </button>
        </div>
      )}
      {choice && section === "minecraft" ? (
        <InstallSelection
          key={`${rootId}:${choice.id}`}
          api={api}
          rootId={rootId}
          version={choice.id}
          native={native}
          disabled={busy || gameBusy || !rootAvailable}
          installed={installed}
          onBack={() => setChoice(null)}
          onStart={start}
        />
      ) : (
        <div className="ce-page-enter" key={section}>
          {section === "favorites" ? (
            <Favorites />
          ) : installerPages.includes(section) ? (
            <LoaderCatalog
              key={section}
              api={api}
              loader={section}
              catalog={catalog}
              catalogLoading={loading}
            />
          ) : community ? (
            <CommunityCatalog
              api={api}
              scopeKey={rootId ?? "preview"}
              key={`${section}:${compatibility?.section === section ? compatibility.sequence : "default"}`}
              compatibility={
                compatibility?.section === section ? compatibility : undefined
              }
              label={community}
              query={query}
              setQuery={setQuery}
              onResourceDetails={onResourceDetails}
            />
          ) : section !== "minecraft" ? (
            <section className="ce-card">
              <div className="ce-card-title">{section}</div>
              <p className="muted">{t("download.catalogUnavailable")}</p>
            </section>
          ) : (
            <>
              <section className="ce-card ce-catalog-latest">
                <div className="ce-card-title">{t("ui.latest")}</div>
                {loading ? (
                  <div className="download-empty">
                    <LoaderCircle className="spin" size={22} />
                    <p>{t("download.catalogLoading")}</p>
                  </div>
                ) : latest.length ? (
                  latest.map((entry) => versionRow(entry, true))
                ) : (
                  <div className="download-empty">
                    <p>
                      {native
                        ? t("download.catalogEmpty")
                        : t("download.catalogDesktop")}
                    </p>
                    <button
                      className="ce-button"
                      disabled={!native || loading}
                      onClick={() => void loadCatalog(true)}
                    >
                      <RefreshCw size={14} />
                      {t("ui.reload")}
                    </button>
                  </div>
                )}
              </section>
              {groups.map((group) => (
                <section className="ce-card ce-version-group" key={group.id}>
                  <button
                    className="ce-version-group-toggle"
                    onClick={() =>
                      setExpanded((old) =>
                        old.includes(group.id)
                          ? old.filter((id) => id !== group.id)
                          : [...old, group.id],
                      )
                    }
                    aria-expanded={expanded.includes(group.id)}
                  >
                    <span>
                      {group.title} ({formatNumber(group.entries.length)})
                    </span>
                    <ChevronDown
                      size={19}
                      className={`ce-disclosure-arrow ${expanded.includes(group.id) ? "is-open" : ""}`}
                    />
                  </button>
                  <Collapse open={expanded.includes(group.id)}>
                    <div className="ce-version-group-list">
                      {group.entries.length ? (
                        group.entries.map((entry) => versionRow(entry))
                      ) : (
                        <p className="muted">{t("ui.noVersions")}</p>
                      )}
                    </div>
                  </Collapse>
                </section>
              ))}
            </>
          )}
        </div>
      )}
    </>
  );
}

// Filter values are provider/UI protocol IDs. Only their rendered labels
// change language; translating the values would change request routing.
const resourceLabelKeys: Record<string, MessageKey> = {
  模组: "resources.mods",
  整合包: "resources.modpacks",
  数据包: "resources.datapacks",
  资源包: "nav.resourcepacks",
  光影包: "nav.shaderpacks",
  世界: "resources.worlds",
  收藏夹: "resources.favorites",
};
const filterLabel = (value: string) =>
  value === "全部"
    ? t("ui.all")
    : value === "默认"
      ? t("common.default")
      : value === "任意"
        ? t("ui.any")
        : value === "更新时间"
          ? t("ui.updated")
          : value === "下载量"
            ? t("ui.downloads")
            : value;

export function CommunityCatalog({
  api,
  label,
  query,
  setQuery,
  onResourceDetails,
  compatibility,
  scopeKey = "default",
}: {
  api: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  label: string;
  scopeKey?: string;
  compatibility?: ResourceBrowseRequest;
  query: string;
  setQuery: (value: string) => void;
  onResourceDetails: (resource: ResourceSummary) => void;
}) {
  const displayLabel = t(resourceLabelKeys[label]);
  const [source, setSource] = useState("全部");
  const [tag, setTag] = useState("全部");
  const [sort, setSort] = useState("默认");
  const [version, setVersion] = useState(compatibility?.version || "任意");
  const [loader, setLoader] = useState(
    compatibility?.loader
      ? {
          fabric: "Fabric",
          forge: "Forge",
          neoforge: "NeoForge",
          quilt: "Quilt",
        }[compatibility.loader] || "任意"
      : "任意",
  );
  const [submittedQuery, setSubmittedQuery] = useState(query);
  const [searchRevision, setSearchRevision] = useState(0);
  const [retryRevision, setRetryRevision] = useState(0);
  const context = {
    api,
    label,
    scopeKey,
    query,
    submittedQuery,
    source,
    tag,
    sort,
    version,
    loader,
    searchRevision,
  };
  const queryOwner = useRef(context);
  if (
    (Object.keys(context) as (keyof typeof context)[]).some(
      (key) => context[key] !== queryOwner.current[key],
    )
  )
    queryOwner.current = context;
  const renderedQueryOwner = queryOwner.current;
  const pages = useRef({
    owner: renderedQueryOwner,
    cache: new CommunityPages(),
  });
  if (pages.current.owner !== renderedQueryOwner)
    pages.current = { owner: renderedQueryOwner, cache: new CommunityPages() };
  const [pageState, setPageState] = useState({
    owner: renderedQueryOwner,
    page: 1,
  });
  // A context change is page one during render, before effects can dispatch an
  // old page or revive handlers after a root/API/filter A → B → A round trip.
  const page = pageState.owner === renderedQueryOwner ? pageState.page : 1;
  const catalogOwner = useRef({
    query: renderedQueryOwner,
    page,
    retryRevision,
  });
  if (
    catalogOwner.current.query !== renderedQueryOwner ||
    catalogOwner.current.page !== page ||
    catalogOwner.current.retryRevision !== retryRevision
  )
    catalogOwner.current = { query: renderedQueryOwner, page, retryRevision };
  const renderedCatalogOwner = catalogOwner.current;
  const resultsElement = useRef<HTMLElement | null>(null);
  const pageScroll = useRef<{ query: object; page: number } | null>(null);
  if (pageScroll.current?.query !== renderedQueryOwner)
    pageScroll.current = null;
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const current = () =>
    live.current && catalogOwner.current === renderedCatalogOwner;
  const [status, setStatus] = useState<{
    owner: object;
    loading: boolean;
    error: string;
  } | null>(null);
  const result = pages.current.cache.get(page);
  const hits = pages.current.cache.visibleHits(page);
  const warnings = result?.warnings ?? [];
  const attributions = [
    ...new Set(
      hits
        .map((hit) => hit.attribution)
        .filter((value): value is string => Boolean(value)),
    ),
  ];
  const pendingSearch = submittedQuery !== query;
  const error = status?.owner === renderedCatalogOwner ? status.error : "";
  const loading =
    !pendingSearch &&
    label !== "世界" &&
    (status?.owner === renderedCatalogOwner ? status.loading : !result);
  const encyclopedia = source === "MC百科";
  const encyclopediaSupported = ["模组", "整合包"].includes(label);
  const projectType = (
    {
      模组: "mod",
      整合包: "modpack",
      数据包: "datapack",
      资源包: "resourcepack",
      光影包: "shader",
    } as Record<string, string>
  )[label];
  useEffect(() => {
    if (label === "世界" || pendingSearch) return;
    let disposed = false;
    const cache = pages.current.cache;
    if (cache.get(page)) {
      setStatus({ owner: renderedCatalogOwner, loading: false, error: "" });
      return;
    }
    setStatus({ owner: renderedCatalogOwner, loading: true, error: "" });
    api<CommunityPage>("community_search", {
      query,
      source,
      page,
      sort: encyclopedia
        ? undefined
        : sort === "更新时间"
          ? "updated"
          : sort === "默认" && query.trim()
            ? "relevance"
            : "downloads",
      version: encyclopedia || version === "任意" ? undefined : version,
      loader:
        encyclopedia || loader === "任意" ? undefined : loader.toLowerCase(),
      tag: encyclopedia || tag === "全部" ? undefined : tag,
      projectType,
    })
      .then((result) => {
        if (disposed || !current()) return;
        if (
          result.pagination &&
          (result.pagination.page !== page ||
            typeof result.pagination.has_next !== "boolean")
        )
          throw Error(t("community.invalidPage"));
        cache.remember(page, { ...result, warnings: result.warnings ?? [] });
        setStatus({ owner: renderedCatalogOwner, loading: false, error: "" });
      })
      .catch((reason) => {
        if (!disposed && current())
          setStatus({
            owner: renderedCatalogOwner,
            loading: false,
            error: String(reason),
          });
      });
    return () => {
      disposed = true;
    };
  }, [renderedCatalogOwner, projectType, pendingSearch]);
  useEffect(() => {
    const intent = pageScroll.current;
    if (
      !intent ||
      intent.query !== renderedQueryOwner ||
      intent.page !== page ||
      !current() ||
      loading ||
      pendingSearch ||
      error ||
      !result
    )
      return;
    pageScroll.current = null;
    const section = resultsElement.current;
    const main = section?.closest<HTMLElement>("main.content");
    if (!section || !main) return;
    // Explicit pagination positions only the launcher's main viewport after
    // the target page is in the DOM. scrollIntoView could also move the window
    // or other ancestors; automatic scrolling avoids an animation surviving a
    // later query change. Focus stays on the user's pagination control.
    const top = main.getBoundingClientRect().top + main.clientTop + 8;
    main.scrollTo({
      top: Math.max(
        0,
        main.scrollTop + section.getBoundingClientRect().top - top,
      ),
      behavior: "auto",
    });
  }, [
    renderedQueryOwner,
    renderedCatalogOwner,
    page,
    loading,
    pendingSearch,
    error,
    result,
  ]);
  function search() {
    if (!current()) return;
    setSubmittedQuery(query);
    setSearchRevision((revision) => revision + 1);
  }
  function changePage(next: number) {
    if (
      !current() ||
      loading ||
      pendingSearch ||
      next < 1 ||
      next > 0xffffffff ||
      (next > page && !result?.pagination?.has_next)
    )
      return;
    pageScroll.current = { query: renderedQueryOwner, page: next };
    setPageState({ owner: renderedQueryOwner, page: next });
  }
  const availableVersions = [
    ...new Set([
      ...hits
        .flatMap((hit) => hit.versions)
        .filter((value) => /^\d+\.\d+(\.\d+)?$/.test(value)),
      ...(compatibility?.version ? [compatibility.version] : []),
      ...(version !== "任意" ? [version] : []),
    ]),
  ].sort((a, b) => b.localeCompare(a, undefined, { numeric: true }));
  const loaderNames: Record<string, string> = {
    fabric: "Fabric",
    forge: "Forge",
    neoforge: "NeoForge",
    quilt: "Quilt",
  };
  const categoryNames: Record<string, string> = {
    library: t("category.library"),
    optimization: t("category.optimization"),
    utility: t("category.utility"),
    decoration: t("category.decoration"),
    adventure: t("category.adventure"),
    technology: t("category.technology"),
    worldgen: t("category.worldgen"),
    storage: t("category.storage"),
    equipment: t("category.equipment"),
    magic: t("category.magic"),
    management: t("nav.manage"),
  };
  const tags = [
    ...new Set([
      ...hits
        .flatMap((hit) => hit.display_categories)
        .filter((value) => !loaderNames[value]),
      ...(tag !== "全部" ? [tag] : []),
    ]),
  ];
  const downloads = (value?: number) =>
    value === undefined
      ? "—"
      : formatNumber(value, { notation: "compact", maximumFractionDigits: 1 });
  const updated = (value?: string) => (value ? formatRelativeDate(value) : "—");
  function showDetails(hit: ResourceSummary) {
    if (!current()) return;
    onResourceDetails({
      ...hit,
      project_type: hit.project_type ?? projectType,
    });
  }

  return (
    <>
      <form
        className="ce-card ce-community-search"
        onSubmit={(event) => {
          event.preventDefault();
          search();
        }}
      >
        <Search size={19} />
        <input
          className="ce-search"
          placeholder={t("ui.searchType", { type: displayLabel })}
          aria-label={t("ui.searchType", { type: displayLabel })}
          value={query}
          onChange={(e) => {
            if (current()) setQuery(e.target.value);
          }}
        />
        <button className="ce-button" type="submit">
          {t("ui.search")}
        </button>
      </form>
      <section className="ce-card ce-community-filters">
        <label className="ce-filter-field">
          {t("ui.source")}
          <CeSelect
            className="ce-field"
            value={source}
            onChange={(e) => {
              if (!current()) return;
              setSource(e.target.value);
              if (e.target.value === "MC百科") {
                setVersion("任意");
                setLoader("任意");
                setTag("全部");
                setSort("默认");
              }
            }}
          >
            {["全部", "Modrinth", "CurseForge", "MC百科"].map((v) => (
              <option
                key={v}
                value={v}
                disabled={v === "MC百科" && !encyclopediaSupported}
              >
                {filterLabel(v)}
              </option>
            ))}
          </CeSelect>
        </label>
        <label className="ce-filter-field">
          {t("ui.tags")}
          <CeSelect
            className="ce-field"
            value={tag}
            disabled={encyclopedia}
            onChange={(e) => {
              if (current() && !encyclopedia) setTag(e.target.value);
            }}
          >
            <option value="全部">{t("ui.all")}</option>
            {tags.map((value) => (
              <option key={value} value={value}>
                {categoryNames[value] || value}
              </option>
            ))}
          </CeSelect>
        </label>
        <label className="ce-filter-field">
          {t("ui.sort")}
          <CeSelect
            className="ce-field"
            value={sort}
            disabled={encyclopedia}
            onChange={(e) => {
              if (current() && !encyclopedia) setSort(e.target.value);
            }}
          >
            {["默认", "下载量", "更新时间"].map((v) => (
              <option key={v} value={v}>
                {filterLabel(v)}
              </option>
            ))}
          </CeSelect>
        </label>
        <button
          className="icon-button ce-filter-reset"
          aria-label={t("ui.resetFilters")}
          onClick={() => {
            if (!current()) return;
            setQuery("");
            setSource("全部");
            setTag("全部");
            setSort("默认");
            setVersion("任意");
            setLoader("任意");
            setSubmittedQuery("");
            setSearchRevision((revision) => revision + 1);
          }}
        >
          <RefreshCw size={22} />
        </button>
        <label className="ce-filter-field">
          {t("ui.version")}
          <CeSelect
            className="ce-field"
            value={version}
            disabled={encyclopedia}
            onChange={(e) => {
              if (current() && !encyclopedia) setVersion(e.target.value);
            }}
          >
            <option value="任意">{t("ui.any")}</option>
            {availableVersions.map((value) => (
              <option key={value}>{value}</option>
            ))}
          </CeSelect>
        </label>
        <label className="ce-filter-field">
          {t("ui.loader")}
          <CeSelect
            className="ce-field"
            value={loader}
            disabled={encyclopedia}
            onChange={(e) => {
              if (current() && !encyclopedia) setLoader(e.target.value);
            }}
          >
            {["任意", "Fabric", "Forge", "NeoForge", "Quilt"].map((v) => (
              <option key={v} value={v}>
                {filterLabel(v)}
              </option>
            ))}
          </CeSelect>
        </label>
      </section>
      {encyclopedia && (
        <p className="ce-community-note" role="status">
          {t("community.mcmodFilters")}
        </p>
      )}
      {!encyclopedia &&
        source === "全部" &&
        (version !== "任意" ||
          loader !== "任意" ||
          tag !== "全部" ||
          sort !== "默认") && (
          <p className="ce-community-note">{t("community.mcmodFallback")}</p>
        )}
      {!loading && warnings.length > 0 && (
        <div className="ce-community-warnings" role="status">
          <TriangleAlert size={15} aria-hidden="true" />
          <div>
            {warnings.map((warning, index) => (
              <p key={`${index}:${warning}`}>{warning}</p>
            ))}
          </div>
        </div>
      )}
      <section
        ref={resultsElement}
        className={
          loading ? "ce-community-loading" : "ce-card ce-community-results"
        }
        aria-busy={loading}
      >
        {loading ? (
          <div className="ce-card ce-community-loading-box" role="status">
            <Pickaxe
              size={38}
              strokeWidth={1.5}
              className="ce-community-loading-pickaxe"
              aria-hidden="true"
            />
            <i aria-hidden="true" />
            <p>{t("download.loadingType", { type: displayLabel })}</p>
          </div>
        ) : pendingSearch ? (
          <div className="download-empty">
            <Search size={24} />
            <p>{t("community.searchPending")}</p>
          </div>
        ) : error ? (
          <div className="download-empty" role="alert">
            <TriangleAlert size={24} />
            <p>{error}</p>
            <button
              className="ce-button"
              onClick={() => {
                if (current()) setRetryRevision((revision) => revision + 1);
              }}
            >
              {t("ui.retry")}
            </button>
          </div>
        ) : !hits.length ? (
          <div className="download-empty">
            <Search size={30} strokeWidth={1.3} />
            <p>
              {result?.hits.length
                ? t("community.pageDuplicates")
                : label === "世界"
                  ? t("download.worldsUnavailable")
                  : t("download.noMatching", { type: displayLabel })}
            </p>
          </div>
        ) : (
          hits.map((hit) => {
            const loaders = hit.categories
              .filter((value) => loaderNames[value])
              .map((value) => loaderNames[value]);
            const releases = hit.versions
              .filter((value) => /^\d+\.\d+(\.\d+)?$/.test(value))
              .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
            const versions =
              releases.length > 1
                ? `${releases[0]} – ${releases[releases.length - 1]}`
                : releases[0] || hit.versions[0] || "";
            const icon = communityIcon(hit.icon_url);
            return (
              <article
                className="ce-mod-row"
                key={
                  hit.identity_key ??
                  `${hit.source ?? "Modrinth"}:${hit.project_id}`
                }
                role="button"
                tabIndex={0}
                aria-label={t("ui.viewDetails", { name: hit.title })}
                onClick={() => showDetails(hit)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    showDetails(hit);
                  }
                }}
              >
                {icon ? (
                  <img
                    className="ce-mod-icon"
                    src={icon}
                    alt=""
                    loading="lazy"
                  />
                ) : (
                  <span className="ce-mod-icon">
                    <Box size={32} />
                  </span>
                )}
                <div className="ce-mod-body">
                  <div className="ce-mod-heading">
                    <div className="ce-mod-title">{hit.title}</div>
                    <span className="ce-community-badges">
                      {communityAliases(hit).map((alias) => (
                        <span
                          className="ce-community-badge"
                          key={`${alias.source}:${alias.project_id ?? alias.url}`}
                        >
                          {alias.source}
                        </span>
                      ))}
                    </span>
                  </div>
                  <div className="ce-mod-description">
                    {hit.display_categories
                      .filter((value) => !loaderNames[value])
                      .slice(0, 2)
                      .map((value) => (
                        <span className="ce-mod-tag" key={value}>
                          {categoryNames[value] || value}
                        </span>
                      ))}
                    <span>{hit.description}</span>
                  </div>
                  <div className="ce-mod-meta">
                    <span>
                      <ListFilter size={13} />
                      <span className="ce-mod-version">
                        {[loaders.join(" / "), versions]
                          .filter(Boolean)
                          .join(", ")}
                      </span>
                    </span>
                    <span>
                      <Download size={13} />
                      {downloads(hit.downloads)}
                    </span>
                    <span
                      title={
                        hit.date_modified
                          ? formatDate(new Date(hit.date_modified))
                          : undefined
                      }
                    >
                      <ArrowUpToLine size={13} />
                      {updated(hit.date_modified)}
                    </span>
                    <span>
                      <Globe size={13} />
                      {hit.source ?? "Modrinth"}
                    </span>
                  </div>
                </div>
              </article>
            );
          })
        )}
      </section>
      {label !== "世界" && (
        <nav
          className="ce-card ce-community-pagination"
          aria-label={t("community.pagination")}
        >
          <button
            type="button"
            className="ce-button"
            disabled={loading || pendingSearch || page === 1}
            onClick={() => changePage(page - 1)}
          >
            {t("community.previousPage")}
          </button>
          <span aria-live="polite">{t("community.currentPage", { page })}</span>
          <button
            type="button"
            className="ce-button"
            disabled={loading || pendingSearch || !result?.pagination?.has_next}
            onClick={() => changePage(page + 1)}
          >
            {t("community.nextPage")}
          </button>
        </nav>
      )}
      {!loading && attributions.length > 0 && (
        <p className="ce-community-note">{attributions.join(" · ")}</p>
      )}
    </>
  );
}
