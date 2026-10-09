import { useEffect, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  FileCode2,
  LockKeyhole,
  Plus,
  Search,
  Layers,
  ArrowUpRight,
} from "lucide-react";
import type { Api } from "./types";
import type { MakerProject } from "./experimentalProjectTypes";
import { experimentalCall as call } from "./experimentalTypes";
import {
  makerKinds,
  type MakerKind,
  type MakerUnit,
  type MakerUnitPage,
  type MakerUnitResult,
} from "./makerWorkspaceState";
import type { MakerActivity, MakerScopeHint } from "./MakerAgentState";
import { useMakerScope } from "./useMakerScope";
import { t, serviceError } from "./i18n";
function fileRole(
  path: string,
): "java" | "model" | "texture" | "language" | "recipe" | "loot" | "file" {
  if (path.endsWith(".java")) return "java";
  if (path.includes("/models/")) return "model";
  if (path.includes("/textures/")) return "texture";
  if (path.includes("/lang/")) return "language";
  if (/\/recipes?\//.test(path)) return "recipe";
  if (path.includes("/loot_table")) return "loot";
  return "file";
}
export function MakerGraph({
  api,
  native,
  project,
  point,
  activity,
  running,
  changed,
  onScope,
  onSource,
  onEdit,
}: {
  api: Api;
  native: boolean;
  project: MakerProject;
  point: import("./experimentalProjectTypes").ProjectCheckpoint;
  activity: MakerActivity[];
  running: boolean;
  changed: string[];
  onScope: (unit: MakerScopeHint) => void;
  onSource: (path?: string) => void;
  onEdit: (unit?: MakerUnit, kind?: MakerKind) => void;
}) {
  const scope = useMakerScope(api, native, `${project.id}:${project.revision}`);
  const [kind, setKind] = useState<MakerKind | null>(null),
    [query, setQuery] = useState(""),
    [searching, setSearching] = useState(false);
  const [counts, setCounts] = useState<MakerUnitPage["counts"] | null>(null),
    [page, setPage] = useState<MakerUnitPage | null>(null),
    [unit, setUnit] = useState<MakerUnit | null>(null);
  const [offset, setOffset] = useState(0),
    [sourceLocks, setSourceLocks] = useState<string[]>([]);
  useEffect(() => {
    setSourceLocks([]);
    const current = scope.readTicket("locks");
    if (native)
      void call<{ fingerprint: string; protectedPaths: string[] }>(
        api,
        "project_protection",
        {
          id: project.id,
          expectedRevision: project.revision,
          checkpointId: point.id,
        },
      )
        .then((value) => {
          if (!current()) return;
          if (value.fingerprint !== point.fingerprint)
            throw new Error(t("maker.responseMismatch"));
          setSourceLocks(value.protectedPaths);
        })
        .catch((e) => {
          if (current()) scope.setError(serviceError(e));
        });
  }, [scope.scope, point.id]);
  const ref = { id: project.id, expectedRevision: project.revision };
  async function list(
    selected: MakerKind | null,
    start: number,
    current = scope.readTicket("graph"),
  ) {
    const value = await call<MakerUnitPage>(api, "project_units", {
      ...ref,
      kind: selected ?? "all",
      query,
      offset: start,
    });
    if (!current()) return;
    if (value.id !== project.id || value.revision !== project.revision)
      throw new Error(t("maker.responseMismatch"));
    setCounts(value.counts);
    setPage(value);
    setOffset(start);
  }
  useEffect(() => {
    setKind(null);
    setUnit(null);
    setPage(null);
    setCounts(null);
    const current = scope.readTicket("graph");
    if (native)
      void list(null, 0, current).catch((e) => {
        if (current()) scope.setError(serviceError(e));
      });
  }, [scope.scope]);
  function selectKind(value: MakerKind) {
    scope.change(() => {
      setKind(kind === value ? null : value);
      setUnit(null);
      setPage(null);
      setOffset(0);
      setSearching(false);
    });
  }
  useEffect(() => {
    if (!native || !kind) return;
    const current = scope.readTicket("graph");
    void list(kind, 0, current).catch((e) => {
      if (current()) scope.setError(serviceError(e));
    });
  }, [kind]);
  async function select(id: string) {
    const current = scope.readTicket("unit");
    const value = await call<MakerUnitResult>(api, "project_unit_read", {
      ...ref,
      unitId: id,
    });
    if (!current()) return;
    if (
      value.id !== project.id ||
      value.revision !== project.revision ||
      value.unit.id !== id
    )
      throw new Error(t("maker.responseMismatch"));
    setUnit(value.unit);
  }
  const reading = running
    ? activity
        .filter(
          (value) =>
            value.name === "workspace.read" && value.status === "running",
        )
        .map((value) => value.path ?? "")
    : [];
  const allLocks = sourceLocks;
  const isRead = (path: string) =>
    reading.some((value) => value.endsWith(`/${path}`));
  const hint = (value: MakerUnit): MakerScopeHint => ({
    id: value.id,
    name: value.name,
    kind: value.kind,
    notes: value.notes,
    files: value.files,
  });
  return (
    <aside className="ma-graph" aria-label={t("agent.graph")}>
      <header className="ma-column-header">
        <div>
          <Layers size={16} />
          <strong>{t("agent.graph")}</strong>
        </div>
        <button
          className="ma-icon"
          aria-label={t("agent.addUnit")}
          disabled={!native || project.archived}
          onClick={() => onEdit(undefined, kind ?? "items")}
        >
          <Plus size={17} />
        </button>
      </header>
      <div className="ma-graph-path">
        {project.name}
        {kind && (
          <>
            {" "}
            <ChevronRight size={12} />
            {t(`maker.kind.${kind}`)}
          </>
        )}
      </div>
      <form
        className="ma-search"
        onSubmit={(e) => {
          e.preventDefault();
          setSearching(true);
          void scope.perform(() => list(kind, 0));
        }}
      >
        <Search size={14} />
        <input
          aria-label={t("agent.findUnit")}
          value={query}
          maxLength={120}
          placeholder={t("agent.findUnit")}
          onChange={(e) =>
            scope.change(() => {
              setQuery(e.target.value);
              setSearching(false);
              setPage(null);
            })
          }
        />
      </form>
      {searching && page && (
        <div className="ma-search-results">
          {page.units.map((value) => (
            <button
              className="ma-unit"
              key={value.id}
              disabled={scope.busy}
              onClick={() => void scope.perform(() => select(value.id))}
            >
              <span>{value.name}</span>
              <small>{t(`maker.kind.${value.kind}`)}</small>
            </button>
          ))}
          {!page.units.length && (
            <p className="ma-muted">{t("agent.noUnits")}</p>
          )}
          {page.nextOffset !== null && (
            <button
              className="ma-text-button"
              disabled={scope.busy}
              onClick={() =>
                void scope.perform(() => list(kind, page.nextOffset!))
              }
            >
              {t("experimental.nextPage")}
            </button>
          )}
        </div>
      )}
      <div className="ma-graph-tree" aria-label={t("agent.domains")}>
        {makerKinds.map((domain) => (
          <div key={domain}>
            <button
              className={`ma-domain ${kind === domain ? "selected" : ""}`}
              aria-expanded={kind === domain}
              draggable
              onDragStart={(e) => {
                e.dataTransfer.setData(
                  "application/x-pcl-maker-unit",
                  JSON.stringify({
                    id: `domain:${domain}`,
                    name: t(`maker.kind.${domain}`),
                    kind: domain,
                    files: [],
                    notes: `Scope: all registered content units in domain ${domain}. Read units as needed; do not claim the whole subtree was read.`,
                  }),
                );
                e.dataTransfer.effectAllowed = "copy";
              }}
              disabled={!native}
              onClick={() => selectKind(domain)}
            >
              {kind === domain ? (
                <ChevronDown size={14} />
              ) : (
                <ChevronRight size={14} />
              )}
              <span>{t(`maker.kind.${domain}`)}</span>
              <small>{counts?.[domain] ?? "—"}</small>
            </button>
            {kind === domain && !searching && (
              <div className="ma-unit-list">
                {page?.units.map((value) => (
                  <button
                    key={value.id}
                    className={`ma-unit ${unit?.id === value.id ? "selected" : ""} ${unit?.id === value.id && unit.files.some(isRead) ? "is-reading" : ""}`}
                    disabled={scope.busy}
                    onClick={() => void scope.perform(() => select(value.id))}
                  >
                    <span>◆</span>
                    <span>{value.name}</span>
                    <small>{value.fileCount}</small>
                    {unit?.id === value.id &&
                      unit.files.some((path) => allLocks.includes(path)) && (
                        <LockKeyhole
                          size={11}
                          aria-label={t("agent.protected")}
                        />
                      )}
                    {unit?.id === value.id &&
                      unit.files.some((path) => changed.includes(path)) ? (
                      <i
                        className="ma-change-dot"
                        aria-label={t("agent.changed")}
                      />
                    ) : null}
                  </button>
                ))}
                {page?.units.length === 0 && (
                  <p className="ma-muted">{t("agent.noUnits")}</p>
                )}
                {page && (
                  <div className="ma-page">
                    <button
                      disabled={!offset || scope.busy}
                      aria-label={t("experimental.previousPage")}
                      onClick={() =>
                        void scope.perform(() =>
                          list(kind, Math.max(0, offset - 50)),
                        )
                      }
                    >
                      ‹
                    </button>
                    <small>
                      {offset + page.units.length} / {page.filteredCount}
                    </small>
                    <button
                      disabled={page.nextOffset === null || scope.busy}
                      aria-label={t("experimental.nextPage")}
                      onClick={() =>
                        void scope.perform(() => list(kind, page.nextOffset!))
                      }
                    >
                      ›
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        ))}
      </div>
      <section className="ma-dossier">
        <span className="ma-eyebrow">{t("agent.dossier")}</span>
        {unit ? (
          <>
            <h3
              draggable
              onDragStart={(e) => {
                e.dataTransfer.setData(
                  "application/x-pcl-maker-unit",
                  JSON.stringify(hint(unit)),
                );
                e.dataTransfer.effectAllowed = "copy";
              }}
            >
              {unit.name}
            </h3>
            <p className="ma-unit-notes">
              {unit.notes || t("agent.noUnitNotes")}
            </p>
            <div className="ma-dossier-actions">
              <button className="ce-button" onClick={() => onScope(hint(unit))}>
                {t("agent.reference")}
              </button>
              <button
                className="ce-button"
                disabled={project.archived}
                onClick={() => onEdit(unit)}
              >
                {t("agent.editDossier")}
              </button>
            </div>
            <div className="ma-dossier-files">
              {unit.files.map((path) => (
                <button
                  key={path}
                  className={isRead(path) ? "is-reading" : ""}
                  title={path}
                  disabled={
                    !/\.(java|json|mcmeta|lang|txt|md|properties|gradle|kts|toml|xml)$/i.test(
                      path,
                    )
                  }
                  onClick={() => onSource(path)}
                >
                  <FileCode2 size={13} />
                  <span>{path.split("/").at(-1)}</span>
                  <em>{t(`agent.fileRole.${fileRole(path)}`)}</em>
                  {allLocks.includes(path) ? (
                    <LockKeyhole size={12} aria-label={t("agent.protected")} />
                  ) : null}
                  {changed.includes(path) && (
                    <i
                      className="ma-change-dot"
                      aria-label={t("agent.changed")}
                    />
                  )}
                </button>
              ))}
            </div>
          </>
        ) : (
          <div className="ma-dossier-empty">
            <FileCode2 size={26} />
            <p>{t("agent.selectUnit")}</p>
            <button className="ma-text-button" onClick={() => onSource()}>
              {t("agent.openSource")}
              <ArrowUpRight size={13} />
            </button>
          </div>
        )}
        <p className="ma-muted ma-graph-note">{t("agent.graphBoundary")}</p>
      </section>
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
    </aside>
  );
}
