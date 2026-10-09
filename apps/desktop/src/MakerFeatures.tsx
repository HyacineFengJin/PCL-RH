import { useEffect, useState } from "react";
import { ChevronRight, FileCode2, Plus, Search, Save } from "lucide-react";
import { CeSelect } from "./CeSelect";
import { t, serviceError } from "./i18n";
import type { Api } from "./types";
import type { MakerProject } from "./experimentalProjectTypes";
import { experimentalCall as call } from "./experimentalTypes";
import {
  makerKinds,
  type MakerDraft,
  type MakerRoute,
  type MakerUnit,
  type MakerUnitPage,
  type MakerUnitResult,
} from "./makerWorkspaceState";
import { useMakerScope } from "./useMakerScope";
export const makerKindLabel = (kind: MakerUnit["kind"]) =>
  t(`maker.kind.${kind}`);
export function MakerFeatures({
  api,
  native,
  project,
  route,
  draft,
  onRoute,
  onContext,
  onSaved,
}: {
  api: Api;
  native: boolean;
  project: MakerProject;
  route: MakerRoute;
  draft: MakerDraft;
  onRoute: (route: MakerRoute) => void;
  onContext: (unit: MakerUnit | undefined) => void;
  onSaved?: (unit: MakerUnit) => void;
}) {
  const key = `${project.id}:${project.revision}:${route.kind ?? "all"}:${route.unitId ?? ""}`;
  const scope = useMakerScope(api, native, key);
  const [page, setPage] = useState<MakerUnitPage | null>(null),
    [query, setQuery] = useState("");
  const [revision, setRevision] = useState(""),
    [unit, setUnit] = useState<MakerUnit | null>(null),
    [paths, setPaths] = useState("");
  const [authoritativeRevision, setAuthoritativeRevision] = useState("");
  const [reload, setReload] = useState(0);
  const draftKey = `${project.id}:${route.unitId ?? "new"}:${route.kind ?? "other"}`;
  const ref = { id: project.id, expectedRevision: project.revision };
  async function list(offset = 0, current = scope.readTicket()) {
    const value = await call<MakerUnitPage>(api, "project_units", {
      ...ref,
      kind: route.kind ?? "all",
      query,
      offset,
    });
    if (!current()) return;
    if (value.id !== project.id || value.revision !== project.revision)
      throw new Error(t("maker.responseMismatch"));
    setPage(value);
    setRevision(value.workspaceRevision);
  }
  useEffect(() => {
    let valid = true;
    setPage(null);
    setUnit(null);
    setPaths("");
    const current = scope.readTicket();
    async function load() {
      if (!native) return;
      if (route.section !== "unit") {
        await list(0, current);
        return;
      }
      const local = draft.units.get(draftKey);
      if (!route.unitId || route.unitId === "new") {
        const index = await call<MakerUnitPage>(api, "project_units", ref);
        if (!valid || !current()) return;
        if (index.id !== project.id || index.revision !== project.revision)
          throw new Error(t("maker.responseMismatch"));
        setAuthoritativeRevision(index.workspaceRevision);
        setRevision(local?.revision ?? index.workspaceRevision);
        const next: MakerUnit = local?.unit ?? {
          id: "",
          name: "",
          kind: route.kind ?? "other",
          state: "planned",
          notes: "",
          files: [],
        };
        setUnit(next);
        setPaths(next.files.join("\n"));
        onContext(next);
      } else {
        const value = await call<MakerUnitResult>(api, "project_unit_read", {
          ...ref,
          unitId: route.unitId,
        });
        if (!valid || !current()) return;
        if (
          value.id !== project.id ||
          value.revision !== project.revision ||
          value.unit.id !== route.unitId
        )
          throw new Error(t("maker.responseMismatch"));
        setAuthoritativeRevision(value.workspaceRevision);
        setRevision(local?.revision ?? value.workspaceRevision);
        const next = local?.unit ?? value.unit;
        setUnit(next);
        setPaths(next.files.join("\n"));
        onContext(next);
      }
    }
    void load().catch((error) => {
      if (valid && current()) scope.setError(serviceError(error));
    });
    return () => {
      valid = false;
    };
  }, [scope.scope, reload]);
  function edit(values: Partial<MakerUnit>) {
    if (!unit) return;
    scope.change(() => {
      const next = { ...unit, ...values };
      setUnit(next);
      draft.units.set(draftKey, { unit: next, revision });
      onContext(next);
    });
  }
  async function save() {
    if (!unit || !revision || project.archived) return;
    const { updatedAt, ...input } = unit;
    const result = await call<MakerUnitResult>(api, "project_unit_save", {
      ...ref,
      expectedWorkspaceRevision: revision,
      unit: input,
    });
    if (!scope.responseCurrent()) return;
    if (
      result.id !== project.id ||
      result.revision !== project.revision ||
      (unit.id && result.unit.id !== unit.id)
    )
      throw new Error(t("maker.responseMismatch"));
    draft.units.delete(draftKey);
    setRevision(result.workspaceRevision);
    setAuthoritativeRevision(result.workspaceRevision);
    setUnit(result.unit);
    onContext(result.unit);
    onSaved?.(result.unit);
    if (!unit.id)
      onRoute({ ...route, unitId: result.unit.id, kind: result.unit.kind });
  }
  const disabled = !native || scope.busy;
  return (
    <div className="maker-document">
      {route.section === "unit" ? (
        <>
          <div className="maker-document-heading">
            <div>
              <small>{makerKindLabel(route.kind ?? "other")}</small>
              <h2>{unit?.name || t("maker.newFeature")}</h2>
            </div>
            <div className="maker-heading-actions">
              <button
                className="ce-button"
                disabled={disabled}
                onClick={() =>
                  scope.change(() => setReload((value) => value + 1))
                }
              >
                {t("maker.refresh")}
              </button>
              <button
                className="ce-button primary"
                disabled={
                  disabled ||
                  project.archived ||
                  !unit?.name.trim() ||
                  !revision
                }
                onClick={() => void scope.perform(save)}
              >
                <Save size={15} />
                {t("maker.saveFeature")}
              </button>
            </div>
          </div>
          {unit && (
            <div className="maker-feature-document">
              {revision !== authoritativeRevision && (
                <button
                  className="ce-button"
                  disabled={disabled || project.archived}
                  onClick={() =>
                    scope.change(() => {
                      setRevision(authoritativeRevision);
                      draft.units.set(draftKey, {
                        unit,
                        revision: authoritativeRevision,
                      });
                    })
                  }
                >
                  {t("maker.acceptFeatureRevision")}
                </button>
              )}
              <label>
                <span>{t("maker.featureName")}</span>
                <input
                  className="ce-field"
                  value={unit.name}
                  maxLength={120}
                  disabled={disabled || project.archived}
                  onChange={(event) => edit({ name: event.target.value })}
                />
              </label>
              <div className="maker-two-fields">
                <label>
                  <span>{t("maker.category")}</span>
                  <CeSelect
                    value={unit.kind}
                    disabled={disabled || project.archived}
                    onChange={(event) =>
                      edit({ kind: event.target.value as MakerUnit["kind"] })
                    }
                  >
                    {makerKinds.map((kind) => (
                      <option key={kind} value={kind}>
                        {makerKindLabel(kind)}
                      </option>
                    ))}
                  </CeSelect>
                </label>
                <label>
                  <span>{t("maker.featureState")}</span>
                  <CeSelect
                    value={unit.state}
                    disabled={disabled || project.archived}
                    onChange={(event) =>
                      edit({ state: event.target.value as MakerUnit["state"] })
                    }
                  >
                    {(["planned", "in_progress", "ready"] as const).map(
                      (state) => (
                        <option key={state} value={state}>
                          {t(`maker.state.${state}`)}
                        </option>
                      ),
                    )}
                  </CeSelect>
                </label>
              </div>
              <label>
                <span>{t("maker.featureRequirements")}</span>
                <textarea
                  className="ce-field maker-notes"
                  rows={10}
                  maxLength={6000}
                  value={unit.notes}
                  disabled={disabled || project.archived}
                  placeholder={t("maker.featurePlaceholder")}
                  onChange={(event) => edit({ notes: event.target.value })}
                />
              </label>
              <label>
                <span>{t("maker.relatedFiles")}</span>
                <textarea
                  className="ce-field maker-code-input"
                  rows={4}
                  maxLength={16416}
                  value={paths}
                  disabled={disabled || project.archived}
                  placeholder="src/main/java/example/Feature.java"
                  onChange={(event) => {
                    if (!scope.callbackCurrent()) return;
                    setPaths(event.target.value);
                    edit({
                      files: event.target.value
                        .split("\n")
                        .map((path) => path.trim())
                        .filter(Boolean),
                    });
                  }}
                />
              </label>
              <p className="experimental-origin">
                {t("maker.featureNotesHelp")}
              </p>
              {!!unit.files.length && (
                <div className="maker-related-list">
                  {unit.files.map((path, index) => (
                    <span key={`${path}:${index}`}>
                      <FileCode2 size={14} />
                      {path}
                    </span>
                  ))}
                </div>
              )}
            </div>
          )}
        </>
      ) : (
        <>
          <div className="maker-document-heading">
            <div>
              <small>{project.name}</small>
              <h2>
                {route.kind ? makerKindLabel(route.kind) : t("maker.features")}
              </h2>
            </div>
            <button
              className="ce-button primary"
              disabled={disabled || project.archived}
              onClick={() => {
                if (scope.callbackCurrent())
                  onRoute({
                    ...route,
                    section: "unit",
                    unitId: "new",
                    kind: route.kind ?? "other",
                  });
              }}
            >
              <Plus size={15} />
              {t("maker.newFeature")}
            </button>
          </div>
          <div className="maker-list-toolbar">
            <Search size={16} />
            <input
              className="ce-field"
              aria-label={t("maker.searchFeatures")}
              value={query}
              maxLength={120}
              disabled={disabled}
              placeholder={t("maker.searchFeatures")}
              onChange={(event) =>
                scope.change(() => {
                  setQuery(event.target.value);
                  setPage(null);
                })
              }
            />
            <button
              className="ce-button"
              disabled={disabled}
              onClick={() => void scope.perform(() => list())}
            >
              {t("maker.search")}
            </button>
          </div>
          <div className="maker-feature-list">
            {page?.units.map((value) => (
              <button
                key={value.id}
                className="maker-feature-row"
                disabled={disabled}
                onClick={() => {
                  if (scope.callbackCurrent())
                    onRoute({
                      projectId: project.id,
                      section: "unit",
                      unitId: value.id,
                      kind: value.kind,
                    });
                }}
              >
                <span className="maker-row-symbol">
                  {value.name.slice(0, 1)}
                </span>
                <span>
                  <strong>{value.name}</strong>
                  <small>
                    {makerKindLabel(value.kind)} ·{" "}
                    {t("maker.linkedFileCount", { count: value.fileCount })}
                  </small>
                </span>
                <span className={`maker-state ${value.state}`}>
                  {t(`maker.state.${value.state}`)}
                </span>
                <ChevronRight size={15} />
              </button>
            ))}
          </div>
          {page && !page.units.length && (
            <div className="maker-empty">
              <h3>{t("maker.noFeatures")}</h3>
              <p>{t("maker.noFeaturesHelp")}</p>
            </div>
          )}
          {page && (
            <div className="maker-pagination">
              <span>
                {t("maker.featureCount", { count: page.filteredCount })}
              </span>
              <button
                className="ce-button"
                disabled={disabled || !page.offset}
                onClick={() =>
                  void scope.perform(() => list(Math.max(0, page.offset - 50)))
                }
              >
                {t("experimental.previousPage")}
              </button>
              <button
                className="ce-button"
                disabled={disabled || page.nextOffset === null}
                onClick={() => void scope.perform(() => list(page.nextOffset!))}
              >
                {t("experimental.nextPage")}
              </button>
            </div>
          )}
        </>
      )}
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
    </div>
  );
}
