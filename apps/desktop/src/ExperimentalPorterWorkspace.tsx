/** Persistent project workflow over the shared host API.
 * Form/source drafts are local until create/configure; project.snapshot is the
 * next baseline, while job.porterSource remains the selected round's input.
 * Reviews capture ownerJob and digest, even when a newer baseline exists.
 * Callbacks and replies share a page/API/selection identity. Retiring the UI
 * never cancels an admitted host transaction, but forbids subsequent UI calls.
 * Host revisions arbitrate user feedback, model tools and recovery writes. */
import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { CeSelect } from "./CeSelect";
import { t, formatDate, serviceError, type MessageKey } from "./i18n";
import type { Api } from "./types";
import { InstanceOperationDialog } from "./instanceOperationUi";
import { ExperimentalVersion } from "./ExperimentalVersion";
import { ExperimentalPorterCompare } from "./ExperimentalPorterCompare";
import type { ExperimentalDraft } from "./ExperimentalTools";
import type { PorterSourceOwner, SourceRelease } from "./porterSourceOwner";
import {
  experimentalCall as call,
  type EngineStatus,
  type JobView,
  type ReviewView,
} from "./experimentalTypes";
import {
  ExperimentalPorter,
  ExperimentalPorterReport,
  ExperimentalPorterPatch,
  ExperimentalPorterDiagnostics,
} from "./ExperimentalPorter";
import {
  identifierProfile,
  type ImportedSource,
  type PorterProject,
  type PorterProjectList,
  type PorterReport,
  type PorterRecipeResult,
  type PorterOrigin,
  type PorterIntake,
} from "./experimentalPorterTypes";

type PorterPageScope = {
  api: Api;
  native: boolean;
  selection: string;
  epoch: number;
};

export function ExperimentalPorterWorkspace({
  api: pageApi,
  native,
  drafts,
  sourceOwner,
  releaseSource,
  onConfigureAi,
  onBrowseMods,
  intake,
  onIntakeConsumed,
}: {
  api: Api;
  native: boolean;
  drafts: RefObject<
    Partial<Record<"maker" | "porter" | "extensions", ExperimentalDraft>>
  >;
  sourceOwner: PorterSourceOwner;
  releaseSource: SourceRelease;
  onConfigureAi?: () => void;
  onBrowseMods?: (origin?: PorterOrigin | null) => void;
  intake?: PorterIntake | null;
  onIntakeConsumed?: () => void;
}) {
  const draft = drafts.current.porter;
  const clearSource = () => {
    void sourceOwner.clear();
    setSource(null);
    if (drafts.current.porter) drafts.current.porter.source = null;
  };
  const [projectId, setProjectId] = useState(draft?.porterProjectId || "");
  const [project, setProject] = useState<PorterProject | null>(null);
  const [listing, setListing] = useState<PorterProjectList | null>(null);
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [newProject, setNewProject] = useState(draft?.porterNew || false);
  const [name, setName] = useState(draft?.porterName || ""),
    [goal, setGoal] = useState(draft?.porterGoal || "");
  const [originUrl, setOriginUrl] = useState(draft?.porterOriginUrl || ""),
    [origin, setOrigin] = useState<PorterOrigin | null>(null);
  const [source, setSource] = useState<ImportedSource | null>(
      draft?.source || null,
    ),
    [directory, setDirectory] = useState(draft?.directory || "");
  const [targetId, setTargetId] = useState(draft?.targetId || "neoforge-26.3"),
    [rights, setRights] = useState(draft?.rights || "unknown");
  const [beta, setBeta] = useState(draft?.beta || false),
    [allowed, setAllowed] = useState<string[]>(draft?.allowed || []);
  const [identifierDeclared, setIdentifierDeclared] = useState(
      draft?.identifierDeclared || false,
    ),
    [mode, setMode] = useState(draft?.mode || "template");
  const [targets, setTargets] = useState<
    { id: string; minecraft: string; loader: string; channel: string }[]
  >([]);
  const [jobId, setJobId] = useState(draft?.jobId || ""),
    [job, setJob] = useState<JobView | null>(null),
    [report, setReport] = useState<PorterReport | null>(null);
  const [result, setResult] = useState<PorterRecipeResult | null>(null),
    [review, setReview] = useState<(ReviewView & { ownerJob: string }) | null>(
      null,
    );
  const [message, setMessage] = useState(""),
    [kind, setKind] = useState("request"),
    [editing, setEditing] = useState(false),
    [editingRevision, setEditingRevision] = useState(0);
  const [operationId, setOperationId] = useState(""),
    [path, setPath] = useState(""),
    [fileText, setFileText] = useState("");
  const [error, setError] = useState("");
  const live = useRef(true),
    working = useRef<{ scope: PorterPageScope } | null>(null),
    epoch = useRef(0);
  const selection = JSON.stringify([
    projectId,
    jobId,
    newProject,
    editing,
    operationId,
    path,
    intake?.id,
    review?.ownerJob,
    review?.reviewId,
    review?.reviewDigest,
  ]);
  const owner = useRef<PorterPageScope>({
    api: pageApi,
    native,
    selection,
    epoch: epoch.current,
  });
  if (
    owner.current.api !== pageApi ||
    owner.current.native !== native ||
    owner.current.selection !== selection ||
    owner.current.epoch !== epoch.current
  )
    owner.current = { api: pageApi, native, selection, epoch: epoch.current };
  const scope = owner.current;
  const current = () =>
    live.current && owner.current === scope && epoch.current === scope.epoch;
  const workingHere = () => working.current?.scope === scope;
  const interactive = () => current() && !workingHere();
  const [busyScope, setBusyScope] = useState<PorterPageScope | null>(null);
  const busy = busyScope === scope;
  // Guard every step of a multi-call action, including calls after an await.
  // The first dispatched host write stays durable when this scope is retired.
  const api = useMemo<Api>(
    () => (command, args) => {
      if (!current() || !scope.native)
        return Promise.reject(new Error("Porter page request retired"));
      return scope.api(command, args);
    },
    [scope],
  );
  function change(action: () => void, retire = false) {
    if (!interactive()) return;
    if (retire) epoch.current++;
    action();
  }
  const active = !!project?.activeRound;
  const artifact = job?.artifacts.find((a) => a.operationId === operationId);
  const legacy = status?.jobs.filter(
    (j) =>
      j.workflow === "porter" &&
      !listing?.projects.some((p) => p.jobIds.includes(j.jobId)),
  );
  useEffect(() => {
    drafts.current.porter = {
      spec: draft?.spec,
      source,
      directory,
      targetId,
      rights,
      beta,
      allowed,
      identifierDeclared,
      mode,
      prompt: goal,
      jobId,
      porterProjectId: projectId,
      porterName: name,
      porterGoal: goal,
      porterOriginUrl: originUrl,
      porterNew: newProject,
    };
  }, [
    source,
    directory,
    targetId,
    rights,
    beta,
    allowed,
    identifierDeclared,
    mode,
    goal,
    jobId,
    projectId,
    name,
    originUrl,
    newProject,
    drafts,
  ]);
  useEffect(() => {
    if (!intake) return;
    epoch.current++;
    setOriginUrl(intake.url);
    setOrigin(null);
    setName(intake.title);
    setNewProject(true);
    setEditing(false);
    setGoal("");
    clearSource();
    setDirectory("");
    setAllowed([]);
    setRights("unknown");
    setBeta(false);
    setIdentifierDeclared(false);
    setError("");
    onIntakeConsumed?.();
  }, [intake?.id]);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  async function refreshList() {
    if (!current()) return;
    const [engine, projects] = await Promise.all([
      call<EngineStatus>(api, "status"),
      call<PorterProjectList>(api, "porter_projects"),
    ]);
    if (current()) {
      setStatus(engine);
      setListing(projects);
    }
  }
  useEffect(() => {
    let valid = true,
      polling = false;
    async function poll() {
      if (!current() || !native || polling || workingHere()) return;
      polling = true;
      try {
        const [engine, projects, selected] = await Promise.all([
          call<EngineStatus>(api, "status"),
          call<PorterProjectList>(api, "porter_projects"),
          projectId
            ? call<PorterProject>(api, "porter_project_read", { projectId })
            : Promise.resolve(null),
        ]);
        if (valid && current() && !workingHere()) {
          setStatus(engine);
          setListing(projects);
          setProject(selected);
          if (selected && !jobId) setJobId(selected.rounds.at(-1)?.jobId || "");
        }
      } catch (e) {
        if (valid && current()) setError(serviceError(e));
      } finally {
        polling = false;
      }
    }
    void poll();
    const timer = window.setInterval(() => void poll(), 2000);
    return () => {
      valid = false;
      window.clearInterval(timer);
    };
  }, [api, native, projectId, jobId]);
  useEffect(() => {
    let valid = true;
    if (native)
      void call<{ targets: typeof targets }>(api, "catalog")
        .then((value) => {
          if (valid && current()) setTargets(value.targets);
        })
        .catch((e) => {
          if (valid && current()) setError(serviceError(e));
        });
    return () => {
      valid = false;
    };
  }, [api, native]);
  useEffect(() => {
    setJob(null);
    setReport(null);
    setResult(null);
    setReview(null);
    setOperationId("");
    setPath("");
  }, [pageApi, native, jobId]);
  useEffect(() => {
    // A new transport must read its own project and list before offering writes.
    setProject(null);
    setListing(null);
    setStatus(null);
  }, [pageApi, native]);
  useEffect(() => {
    let valid = true,
      polling = false;
    async function poll() {
      if (!current() || !native || !jobId || polling || workingHere()) return;
      polling = true;
      try {
        const value = await call<JobView>(api, "job_read", { jobId });
        if (value.summary.workflow !== "porter")
          throw new Error("Wrong workflow");
        const analysis = ["queued", "running"].includes(value.summary.status)
          ? null
          : await call<PorterReport | null>(api, "report_read", { jobId });
        if (valid && current() && !workingHere()) {
          setJob(value);
          setReport(analysis);
          setOperationId(
            (old) => old || value.artifacts.at(-1)?.operationId || "",
          );
        }
      } catch (e) {
        if (valid && current()) setError(serviceError(e));
      } finally {
        polling = false;
      }
    }
    void poll();
    const timer = window.setInterval(() => void poll(), 1800);
    return () => {
      valid = false;
      window.clearInterval(timer);
    };
  }, [api, native, jobId]);
  useEffect(() => {
    setPath(artifact?.files[0] || "");
  }, [artifact?.operationId]);
  useEffect(() => {
    let valid = true;
    setFileText("");
    if (native && jobId && operationId && path)
      void call<{ content: string | null }>(api, "artifact_read", {
        jobId,
        operationId,
        path,
      })
        .then((v) => {
          if (valid && current()) setFileText(v.content || "");
        })
        .catch((e) => {
          if (valid && current()) setError(serviceError(e));
        });
    return () => {
      valid = false;
    };
  }, [api, native, jobId, operationId, path]);
  async function perform<T>(
    action: () => Promise<T>,
    accept?: (value: T) => void,
    discard?: (value: T) => void,
  ) {
    if (!interactive() || !native) return;
    const run = { scope };
    working.current = run;
    setBusyScope(scope);
    setError("");
    try {
      const value = await action();
      if (current()) {
        accept?.(value);
        await refreshList();
      } else discard?.(value);
    } catch (e) {
      if (current()) {
        setError(serviceError(e));
        if (projectId)
          void call<PorterProject>(api, "porter_project_read", { projectId })
            .then((p) => {
              if (current()) setProject(p);
            })
            .catch(() => {});
      }
    } finally {
      if (working.current === run) working.current = null;
      if (current()) setBusyScope(null);
    }
  }
  function chooseProject(id: string) {
    if (!interactive()) return;
    epoch.current++;
    setProjectId(id);
    setProject(null);
    setJobId("");
    setNewProject(false);
    setEditing(false);
    setMessage("");
  }
  function chooseJob(id: string) {
    if (!interactive() || id === jobId) return;
    commitJobSelection(id);
  }
  // Accepted host replies may select their own round while the UI is busy.
  // Public selection handlers must go through interactive() first.
  function commitJobSelection(id: string) {
    epoch.current++;
    setJobId(id);
  }
  function chooseLegacyJob(id: string) {
    if (!interactive()) return;
    chooseProject("");
    setJobId(id);
  }
  function beginNewProject() {
    if (!interactive() || newProject) return;
    epoch.current++;
    setNewProject(true);
    setEditing(false);
    setOriginUrl("");
    setOrigin(null);
    setName("");
    setGoal("");
    clearSource();
    setDirectory("");
    setAllowed([]);
    setRights("unknown");
    setBeta(false);
    setIdentifierDeclared(false);
    setError("");
  }
  async function reviewFor(ownerJob: string, id: string) {
    return {
      ...(await call<ReviewView>(api, "review_read", {
        jobId: ownerJob,
        reviewId: id,
      })),
      ownerJob,
    };
  }
  function fillSettings() {
    if (!interactive() || !project) return;
    epoch.current++;
    setName(project.name);
    setGoal(project.goal);
    setTargetId(project.targetId);
    setRights(project.rights);
    setBeta(project.acknowledgeBeta);
    setIdentifierDeclared(project.identifierProfile === identifierProfile);
    clearSource();
    setDirectory("");
    setAllowed(project.snapshot?.permittedPaths || []);
    setEditingRevision(project.revision);
    setEditing(true);
  }
  const intakeSource =
    source ||
    (editing && project?.snapshot
      ? { sourceId: "", files: project.snapshot.files, skipped: [] }
      : null);
  const configuration = {
    name,
    goal,
    targetId,
    rights,
    acknowledgeBeta: beta,
    identifierProfile:
      targetId === "fabric-1.21-yarn-source" && identifierDeclared
        ? identifierProfile
        : null,
    ...(source ? { sourceId: source.sourceId } : {}),
    permittedPaths: allowed,
  };
  const question = project?.messages.find(
    (m) => m.id === project.awaitingQuestion,
  );
  const disabled = busy || !native;
  const capacity =
    (status?.jobs.filter((j) => ["queued", "running"].includes(j.status))
      .length || 0) >= 2;
  const modeControl = (
    <label className="ce-row">
      <span>{t("experimental.mode")}</span>
      <CeSelect
        className="ce-field"
        value={mode}
        disabled={disabled}
        onChange={(e) => change(() => setMode(e.target.value))}
      >
        <option value="template">{t("experimental.template")}</option>
        <option value="live" disabled={!status?.liveAvailable}>
          {t("experimental.live")}
        </option>
      </CeSelect>
    </label>
  );
  return (
    <div className="experimental-panel">
      <section className="ce-card">
        <h2 className="ce-card-title">{t("experimental.porterManagement")}</h2>
        <p>{t("experimental.porterManagementHelp")}</p>
        <div className="ce-actions">
          <button
            className="ce-button primary"
            disabled={disabled}
            onClick={beginNewProject}
          >
            {t("experimental.porterNewProject")}
          </button>
          <button
            className="ce-button"
            disabled={busy || !onBrowseMods}
            onClick={() => change(() => onBrowseMods?.())}
          >
            {t("experimental.porterBrowseMods")}
          </button>
          <button
            className="ce-button"
            disabled={busy || !onConfigureAi}
            onClick={() => change(() => onConfigureAi?.())}
          >
            {t("experimental.ai")}
          </button>
        </div>
        <div className="experimental-job-list">
          {listing?.projects.map((p) => (
            <button
              className={`ce-button experimental-job ${p.id === projectId ? "primary" : ""}`}
              disabled={busy}
              key={p.id}
              onClick={() => chooseProject(p.id)}
            >
              <span>
                {p.name} · {p.targetId}
              </span>
              <span>
                {t(
                  p.archived
                    ? "experimental.porterArchived"
                    : p.activeRound
                      ? "experimental.job.running"
                      : p.awaitingQuestion
                        ? "experimental.porterAwaiting"
                        : !p.hasSource
                          ? "experimental.porterNeedsSource"
                          : "experimental.porterReviewRequired",
                )}
              </span>
            </button>
          ))}
        </div>
        {!listing?.projects.length && (
          <p className="experimental-origin">
            {t("experimental.porterNoProjects")}
          </p>
        )}
        {!!listing?.unreadableProjectIds.length && (
          <p className="experimental-error">
            {t("experimental.porterUnreadable", {
              count: listing.unreadableProjectIds.length,
            })}
          </p>
        )}
        {!!legacy?.length && (
          <details className="experimental-safe">
            <summary>{t("experimental.porterPreviousJobs")}</summary>
            <div className="experimental-job-list">
              {legacy.map((j) => (
                <button
                  className="ce-button experimental-job"
                  key={j.jobId}
                  disabled={busy}
                  onClick={() => chooseLegacyJob(j.jobId)}
                >
                  <span>
                    {formatDate(j.updatedAt)} · {j.jobId.slice(0, 8)}
                  </span>
                </button>
              ))}
            </div>
          </details>
        )}
      </section>
      {(newProject || editing) && (
        <>
          <section className="ce-card">
            <h2 className="ce-card-title">
              {t(
                editing
                  ? "experimental.porterConfiguration"
                  : "experimental.porterNewProject",
              )}
            </h2>
            <label className="ce-row">
              <span>{t("experimental.porterProjectName")}</span>
              <input
                className="ce-field"
                value={name}
                maxLength={160}
                disabled={disabled}
                onChange={(e) => change(() => setName(e.target.value))}
              />
            </label>
            <label className="ce-row">
              <span>{t("experimental.porterGoal")}</span>
              <textarea
                className="ce-field"
                value={goal}
                maxLength={2000}
                rows={3}
                disabled={disabled}
                onChange={(e) => change(() => setGoal(e.target.value))}
              />
            </label>
            {!editing && (
              <>
                <label className="ce-row">
                  <span>{t("experimental.porterOriginLink")}</span>
                  <input
                    className="ce-field"
                    value={originUrl}
                    maxLength={1000}
                    placeholder="Modrinth / CurseForge / MC百科"
                    disabled={disabled}
                    onChange={(e) =>
                      change(() => {
                        setOriginUrl(e.target.value);
                        setOrigin(null);
                      })
                    }
                  />
                </label>
                <div className="ce-actions">
                  <button
                    className="ce-button"
                    disabled={disabled || !originUrl.trim()}
                    onClick={() =>
                      void perform(
                        () =>
                          call<PorterOrigin>(api, "porter_origin_resolve", {
                            url: originUrl.trim(),
                          }),
                        (v) => {
                          setOrigin(v);
                          setOriginUrl(v.url);
                        },
                      )
                    }
                  >
                    {t("experimental.porterResolveLink")}
                  </button>
                  <button
                    className="ce-button"
                    disabled={busy || !onBrowseMods}
                    onClick={() => change(() => onBrowseMods?.(origin))}
                  >
                    {t("experimental.porterBrowseMods")}
                  </button>
                </div>
                {origin && (
                  <>
                    <p className="experimental-origin">
                      {origin.title || origin.projectId} · {origin.provider} ·{" "}
                      {origin.versionId || t("common.none")}
                    </p>
                    {origin.sourceUrl && (
                      <p className="experimental-origin">
                        {t("experimental.porterSourceReference")}:{" "}
                        <a
                          href={origin.sourceUrl}
                          target="_blank"
                          rel="noreferrer"
                        >
                          {origin.sourceUrl}
                        </a>
                      </p>
                    )}
                    <p className="experimental-origin">
                      {t(
                        origin.resolution === "reference-only"
                          ? "experimental.porterLinkReferenceOnly"
                          : "experimental.porterSourceStillRequired",
                      )}
                    </p>
                  </>
                )}
              </>
            )}
          </section>
          <ExperimentalPorter
            source={intakeSource}
            directory={directory}
            targets={targets}
            targetId={targetId}
            rights={rights}
            beta={beta}
            allowed={allowed}
            identifierDeclared={identifierDeclared}
            disabled={disabled}
            onTarget={(v) =>
              change(() => {
                setTargetId(v);
                setIdentifierDeclared(false);
              })
            }
            onRights={(v) => change(() => setRights(v))}
            onBeta={(v) => change(() => setBeta(v))}
            onAllowed={(v) => change(() => setAllowed(v))}
            onIdentifierDeclared={(v) => change(() => setIdentifierDeclared(v))}
            onImport={() =>
              void perform(
                async () => {
                  const picked = await api<{
                    status: string;
                    path?: string;
                    message?: string;
                  }>("experimental_choose", { kind: "source" });
                  if (picked.status === "unavailable")
                    throw new Error(picked.message);
                  return picked.status === "selected"
                    ? {
                        imported: await call<ImportedSource>(
                          api,
                          "source_import",
                          { directory: picked.path },
                        ),
                        directory: picked.path || "",
                      }
                    : null;
                },
                (v) => {
                  if (v) {
                    if (!sourceOwner.adopt(v.imported.sourceId, releaseSource))
                      return;
                    setSource(v.imported);
                    setDirectory(v.directory);
                    setAllowed([]);
                    setIdentifierDeclared(false);
                  }
                },
                (v) => {
                  if (v)
                    void sourceOwner.discard(
                      v.imported.sourceId,
                      releaseSource,
                    );
                },
              )
            }
          />
          <section className="ce-card">
            {modeControl}
            <div className="ce-actions">
              <button
                className="ce-button primary"
                disabled={
                  disabled ||
                  !name.trim() ||
                  !goal.trim() ||
                  (!editing && !source && !originUrl.trim()) ||
                  (editing && active) ||
                  (!editing && !!source && capacity)
                }
                onClick={() =>
                  void perform(
                    async () => {
                      if (editing && project)
                        return {
                          project: await call<PorterProject>(
                            api,
                            "porter_project_configure",
                            {
                              projectId,
                              revision: editingRevision,
                              ...configuration,
                            },
                          ),
                          jobId: "",
                        };
                      const p = await call<PorterProject>(
                        api,
                        "porter_project_create",
                        {
                          ...configuration,
                          ...(originUrl.trim()
                            ? { origin: { ...origin, url: originUrl.trim() } }
                            : {}),
                        },
                      );
                      // The project is already durable if admission fails. Return it with
                      // the error so a retry cannot accidentally create another project.
                      try {
                        const round = source
                          ? await call<{ jobId: string }>(
                              api,
                              "porter_project_round",
                              { projectId: p.id, revision: p.revision, mode },
                            )
                          : null;
                        return {
                          project: round
                            ? await call<PorterProject>(
                                api,
                                "porter_project_read",
                                { projectId: p.id },
                              )
                            : p,
                          jobId: round?.jobId || "",
                        };
                      } catch (e) {
                        return {
                          project: p,
                          jobId: "",
                          startError: serviceError(e),
                        };
                      }
                    },
                    (v) => {
                      setProjectId(v.project.id);
                      setProject(v.project);
                      setNewProject(false);
                      setEditing(false);
                      clearSource();
                      setDirectory("");
                      if (!editing) commitJobSelection(v.jobId);
                      if ("startError" in v) setError(String(v.startError));
                    },
                  )
                }
              >
                {t(
                  editing
                    ? "experimental.porterSaveConfiguration"
                    : source
                      ? "experimental.porterCreateAndStart"
                      : "experimental.porterCreateWaiting",
                )}
              </button>
              <button
                className="ce-button"
                disabled={busy}
                onClick={() =>
                  change(() => {
                    setNewProject(false);
                    setEditing(false);
                  }, true)
                }
              >
                {t("common.cancel")}
              </button>
            </div>
          </section>
        </>
      )}
      {project && !newProject && !editing && (
        <>
          <section className="ce-card">
            <h2 className="ce-card-title">{project.name}</h2>
            <p className="experimental-body">{project.goal}</p>
            <p className="experimental-origin">
              {project.targetId} · {t("experimental.porterBaseline")}:{" "}
              {project.snapshot?.revision || 0}
            </p>
            {project.origin && (
              <p className="experimental-origin">
                {project.origin.provider} · {project.origin.url}
              </p>
            )}
            {project.origin?.sourceUrl && (
              <p className="experimental-origin">
                {t("experimental.porterSourceReference")}:{" "}
                {project.origin.sourceUrl}
              </p>
            )}
            {!project.snapshot && <p>{t("experimental.porterNeedsSource")}</p>}
            {modeControl}
            <div className="ce-actions">
              <button
                className="ce-button primary"
                disabled={
                  disabled ||
                  active ||
                  !project.snapshot ||
                  !!question ||
                  project.archived ||
                  capacity ||
                  (mode === "live" && !status?.liveAvailable)
                }
                onClick={() =>
                  void perform(
                    async () => {
                      const round = await call<{ jobId: string }>(
                        api,
                        "porter_project_round",
                        { projectId, revision: project.revision, mode },
                      );
                      return {
                        ...round,
                        project: await call<PorterProject>(
                          api,
                          "porter_project_read",
                          { projectId },
                        ),
                      };
                    },
                    (v) => {
                      setProject(v.project);
                      commitJobSelection(v.jobId);
                    },
                  )
                }
              >
                {t("experimental.porterContinue")}
              </button>
              <button
                className="ce-button"
                disabled={disabled || active || project.archived}
                onClick={fillSettings}
              >
                {t("experimental.porterConfiguration")}
              </button>
              <button
                className="ce-button"
                disabled={busy || !onBrowseMods}
                onClick={() => change(() => onBrowseMods?.(project.origin))}
              >
                {t("experimental.porterBrowseMods")}
              </button>
              <button
                className="ce-button"
                disabled={disabled || active}
                onClick={() =>
                  void perform(
                    () =>
                      call<PorterProject>(api, "porter_project_archive", {
                        projectId,
                        revision: project.revision,
                        archived: !project.archived,
                      }),
                    setProject,
                  )
                }
              >
                {t(
                  project.archived
                    ? "experimental.porterUnarchive"
                    : "experimental.porterArchive",
                )}
              </button>
            </div>
          </section>
          <section className="ce-card">
            <h2 className="ce-card-title">
              {t("experimental.porterDiscussion")}
            </h2>
            <p className="experimental-origin">
              {t("experimental.porterDiscussionHelp")}
            </p>
            {project.messages.map((m) => (
              <div
                className={`experimental-porter-message experimental-porter-${m.role}`}
                key={m.id}
              >
                <p className="experimental-origin">
                  {t(
                    m.role === "user"
                      ? "experimental.porterUser"
                      : m.role === "assistant"
                        ? "experimental.porterAi"
                        : "experimental.porterRecord",
                  )}{" "}
                  · {formatDate(m.createdAt)}
                  {m.kind === "bug" ? " · BUG" : ""}
                </p>
                <p className="experimental-body">{m.content}</p>
                {m.details &&
                  (["completed", "remaining", "limitations"] as const).map(
                    (key) => (
                      <div key={key}>
                        <strong>
                          {t(`experimental.porter.${key}` as MessageKey)}
                        </strong>
                        {m.details![key].map((item, i) => (
                          <p className="experimental-body" key={i}>
                            {item}
                          </p>
                        ))}
                      </div>
                    ),
                  )}
                {m.role === "assistant" && (
                  <p className="experimental-origin">
                    {t("experimental.porterModelUnverified")}
                  </p>
                )}
                {m.role === "user" && ["request", "bug"].includes(m.kind) && (
                  <button
                    className="ce-button"
                    disabled={disabled || project.archived}
                    onClick={() =>
                      void perform(
                        () =>
                          call<PorterProject>(
                            api,
                            "porter_project_message_resolve",
                            {
                              projectId,
                              revision: project.revision,
                              messageId: m.id,
                              resolved: !m.resolved,
                            },
                          ),
                        setProject,
                      )
                    }
                  >
                    {t(
                      m.resolved
                        ? "experimental.porterReopenIssue"
                        : "experimental.porterResolveIssue",
                    )}
                  </button>
                )}
              </div>
            ))}
            {question && (
              <div className="experimental-safe">
                <strong>{t("experimental.porterAwaiting")}</strong>
                <p className="experimental-body">{question.content}</p>
                <div className="ce-actions">
                  {question.options?.map((option, i) => (
                    <button
                      className="ce-button"
                      disabled={disabled || project.archived}
                      key={i}
                      onClick={() => change(() => setMessage(option))}
                    >
                      {option}
                    </button>
                  ))}
                </div>
              </div>
            )}
            <label className="ce-row">
              <span>{t("experimental.porterMessageKind")}</span>
              <CeSelect
                className="ce-field"
                value={kind}
                disabled={disabled}
                onChange={(e) => change(() => setKind(e.target.value))}
              >
                <option value="request">
                  {t("experimental.porterRequest")}
                </option>
                <option value="bug">BUG</option>
                <option value="feedback">
                  {t("experimental.porterFeedback")}
                </option>
              </CeSelect>
            </label>
            <textarea
              className="ce-field"
              rows={4}
              maxLength={8000}
              value={message}
              disabled={disabled || project.archived}
              aria-label={t("experimental.porterDiscussion")}
              onChange={(e) => change(() => setMessage(e.target.value))}
            />
            {active && (
              <p className="experimental-origin">
                {t("experimental.porterNextRound")}
              </p>
            )}
            <button
              className="ce-button"
              disabled={disabled || project.archived || !message.trim()}
              onClick={() =>
                void perform(
                  () =>
                    call<PorterProject>(api, "porter_project_message", {
                      projectId,
                      revision: project.revision,
                      content: message,
                      kind: question ? "answer" : kind,
                      replyTo: question?.id || null,
                    }),
                  (p) => {
                    setProject(p);
                    setMessage("");
                  },
                )
              }
            >
              {t(
                question
                  ? "experimental.porterAnswer"
                  : "experimental.porterSend",
              )}
            </button>
          </section>
          <section className="ce-card">
            <h2 className="ce-card-title">{t("experimental.porterRounds")}</h2>
            <div className="experimental-job-list">
              {project.rounds.map((r, i) => (
                <button
                  className={`ce-button experimental-job ${r.jobId === jobId ? "primary" : ""}`}
                  disabled={busy}
                  key={r.id}
                  onClick={() => chooseJob(r.jobId)}
                >
                  <span>
                    {i + 1} · {r.targetId} · {t("experimental.porterBaseline")}{" "}
                    {r.baselineRevision}
                  </span>
                  <span>{t(`experimental.job.${r.status}` as MessageKey)}</span>
                </button>
              ))}
            </div>
          </section>
        </>
      )}
      {project && !newProject && !editing && (
        <ExperimentalPorterCompare
          api={api}
          native={native}
          projectId={projectId}
          disabled={busy}
        />
      )}
      {job && !newProject && !editing && (
        <section className="ce-card">
          <h2 className="ce-card-title">
            {t(`experimental.job.${job.summary.status}` as MessageKey)}
          </h2>
          <p>{t("experimental.porterValidationBoundary")}</p>
          {job.result != null && (
            <details>
              <summary>{t("experimental.result")}</summary>
              <pre className="experimental-output">
                {JSON.stringify(job.result, null, 2)}
              </pre>
            </details>
          )}
          {job.error && (
            <p className="experimental-error">{job.error.message}</p>
          )}
          {["queued", "running"].includes(job.summary.status) && (
            <button
              className="ce-button"
              disabled={disabled}
              onClick={() =>
                void perform(() => call(api, "job_cancel", { jobId }))
              }
            >
              {t("experimental.cancelJob")}
            </button>
          )}
          <div className="ce-actions">
            {job.reviews.map((id) => (
              <button
                className="ce-button"
                key={id}
                disabled={
                  disabled || ["queued", "running"].includes(job.summary.status)
                }
                onClick={() =>
                  void perform(() => reviewFor(jobId, id), setReview)
                }
              >
                {t("experimental.review")} · {id.slice(0, 8)}
              </button>
            ))}
          </div>
        </section>
      )}
      {report && !newProject && !editing && (
        <ExperimentalPorterReport report={report} />
      )}
      {job &&
        !newProject &&
        !editing &&
        !["queued", "running"].includes(job.summary.status) && (
          <ExperimentalPorterPatch
            key={jobId}
            source={job.porterSource}
            disabled={disabled}
            result={result}
            onRecipe={(recipe) =>
              void perform(
                async () => {
                  const value = await call<PorterRecipeResult>(
                    api,
                    "porter_recipe",
                    { jobId, recipe },
                  );
                  return {
                    value,
                    review: value.review
                      ? await reviewFor(jobId, value.review.reviewId)
                      : null,
                  };
                },
                (v) => {
                  setResult(v.value);
                  if (v.review) setReview(v.review);
                },
              )
            }
            onPreview={(path, text, purpose) =>
              void perform(async () => {
                const r = await call<{ reviewId: string }>(
                  api,
                  "porter_review",
                  { jobId, path, text, purpose },
                );
                return reviewFor(jobId, r.reviewId);
              }, setReview)
            }
          />
        )}
      {!!job?.artifacts.length && !newProject && !editing && (
        <section className="ce-card">
          <h2 className="ce-card-title">{t("experimental.porterCopies")}</h2>
          <label className="ce-row">
            <span>{t("experimental.artifact")}</span>
            <CeSelect
              className="ce-field"
              value={operationId}
              disabled={busy}
              onChange={(e) =>
                e.target.value !== operationId &&
                change(() => setOperationId(e.target.value), true)
              }
            >
              {job.artifacts.map((a) => (
                <option key={a.operationId} value={a.operationId}>
                  {a.directory}
                </option>
              ))}
            </CeSelect>
          </label>
          <label className="ce-row">
            <span>{t("experimental.file")}</span>
            <CeSelect
              className="ce-field"
              value={path}
              disabled={busy}
              onChange={(e) =>
                e.target.value !== path &&
                change(() => setPath(e.target.value), true)
              }
            >
              {artifact?.files.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </CeSelect>
          </label>
          <textarea
            className="ce-field experimental-editor"
            readOnly
            aria-label={t("experimental.file")}
            value={fileText}
          />
          <div className="ce-actions">
            <button
              className="ce-button"
              disabled={disabled || !artifact}
              onClick={() =>
                void perform(() =>
                  api("experimental_open", { jobId, operationId }),
                )
              }
            >
              {t("experimental.openProject")}
            </button>
            {project && (
              <button
                className="ce-button"
                disabled={
                  disabled ||
                  active ||
                  project.archived ||
                  !artifact ||
                  !project.rounds.some((r) => r.jobId === jobId)
                }
                onClick={() =>
                  void perform(
                    () =>
                      call<PorterProject>(api, "porter_project_source", {
                        projectId,
                        revision: project.revision,
                        jobId,
                        operationId,
                        permittedPaths: job.porterSource?.permittedPaths || [],
                      }),
                    setProject,
                  )
                }
              >
                {t("experimental.porterUseCopy")}
              </button>
            )}
          </div>
          <p className="experimental-origin">
            {t("experimental.porterUseCopyHelp")}
          </p>
        </section>
      )}
      {review && (
        <InstanceOperationDialog
          title={t("experimental.review")}
          titleId="porter-review-title"
          busy={busy}
          committing={busy}
          confirmLabel={t("experimental.apply")}
          confirmDisabled={!native || review.status !== "pending"}
          onClose={() => {
            if (!interactive()) return;
            void call(api, "review_cancel", {
              jobId: review.ownerJob,
              reviewId: review.reviewId,
            }).catch(() => {});
            epoch.current++;
            setReview(null);
          }}
          onConfirm={() =>
            void perform(
              async () => {
                await call(api, "review_apply", {
                  jobId: review.ownerJob,
                  reviewId: review.reviewId,
                  digest: review.reviewDigest,
                });
                return call<JobView>(api, "job_read", {
                  jobId: review.ownerJob,
                });
              },
              (value) => {
                setReview(null);
                setJob(value);
                setOperationId(value.artifacts.at(-1)?.operationId || "");
              },
            )
          }
        >
          <p>{t("experimental.porterCopyBoundary")}</p>
          <ExperimentalPorterDiagnostics
            result={review.preview.domain_recipe}
          />
          <p className="experimental-origin experimental-digest">
            SHA-256: {review.reviewDigest}
          </p>
          {(review.preview.changes || []).map((c, i) => (
            <div className="experimental-review-file" key={i}>
              <strong>{c.path}</strong>
              <pre className="experimental-output">{c.unified_diff}</pre>
            </div>
          ))}
          {error && <p className="experimental-error">{error}</p>}
        </InstanceOperationDialog>
      )}
      {error && (
        <p className="experimental-error" role="alert">
          {error}
        </p>
      )}
      <ExperimentalVersion version="0.6.0" />
    </div>
  );
}
