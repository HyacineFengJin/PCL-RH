/** Agent workbench: project selection, persisted job threads and scope graph.
 * The host owns execution, permissions and immutable source versions. Local
 * composer/scope drafts survive navigation; reads cannot reset user input. */
import { useEffect, useRef, useState, type RefObject } from "react";
import {
  ArrowLeft,
  ArrowUp,
  BookOpen,
  Check,
  Circle,
  Diamond,
  GitBranch,
  Layers,
  MessageSquare,
  Plus,
  Search,
  Settings2,
  Sparkles,
  X,
} from "lucide-react";
import { CeSelect } from "./CeSelect";
import { InstanceOperationDialog } from "./instanceOperationUi";
import { t, formatDate, serviceError } from "./i18n";
import type { Api } from "./types";
import {
  experimentalCall as call,
  type EngineStatus,
} from "./experimentalTypes";
import type {
  MakerProject,
  ProjectSourceSelection,
} from "./experimentalProjectTypes";
import { MakerSource } from "./MakerSource";
import { MakerFeatures } from "./MakerFeatures";
import { MakerCharter, MakerVersions } from "./MakerProjectPanels";
import { ExperimentalVersion } from "./ExperimentalVersion";
import { MakerRegisterProject } from "./MakerRegisterProject";
import { MakerMentions } from "./MakerMentions";
import { MakerGraph } from "./MakerGraph";
import { MakerThreadView } from "./MakerThreadView";
import {
  scopePrompt,
  threadGroup,
  type MakerMode,
  type MakerThread,
  type MakerScopeHint,
  type MakerActivity,
} from "./MakerAgentState";
import {
  createProjectManagerDraft,
  type MakerDraft,
  type MakerKind,
  type MakerUnit,
} from "./makerWorkspaceState";
import { useMakerScope } from "./useMakerScope";
import "./experimental.css";
import "./maker-workspace.css";
export { createProjectManagerDraft } from "./makerWorkspaceState";
type ThreadList = {
  threads: MakerThread[];
  errors: string[];
  pendingJobs: string[];
};
type Overlay =
  | { type: "source"; jobId: string; operationId: string; path?: string }
  | { type: "diff"; title: string; text: string }
  | { type: "charter" }
  | { type: "versions" }
  | { type: "unit"; unitId: string; kind: MakerKind };
export function ExperimentalProjects({
  api,
  native,
  draft: retained,
  onConfigureAi,
}: {
  api: Api;
  native: boolean;
  draft: RefObject<MakerDraft>;
  onConfigureAi?: () => void;
  onOpen?: (source: ProjectSourceSelection) => void;
}) {
  const draft = retained.current,
    scope = useMakerScope(api, native, "maker-agent");
  const [projectId, setProjectId] = useState(draft.agentProject),
    [threadId, setThreadId] = useState(draft.agentThread),
    [mode, setMode] = useState<MakerMode>(draft.agentMode);
  const [projects, setProjects] = useState<MakerProject[]>([]),
    [threads, setThreads] = useState<MakerThread[]>([]),
    [thread, setThread] = useState<MakerThread | null>(null),
    [engine, setEngine] = useState<EngineStatus | null>(null);
  const [pendingJobs, setPendingJobs] = useState(new Set<string>()),
    [query, setQuery] = useState(""),
    [library, setLibrary] = useState(!draft.agentProject && !draft.agentThread),
    [libraryPage, setLibraryPage] = useState(0);
  const [prompt, setPrompt] = useState(
      draft.prompts.get(draft.agentThread || `new:${draft.agentProject}`) ?? "",
    ),
    [hints, setHints] = useState<MakerScopeHint[]>(
      draft.agentScopes.get(draft.agentThread || `new:${draft.agentProject}`) ??
        [],
    );
  const [graphEpoch, setGraphEpoch] = useState(0);
  const [activity, setActivity] = useState<MakerActivity[]>([]),
    [agentRunning, setAgentRunning] = useState(false),
    [overlay, setOverlay] = useState<Overlay | null>(null),
    [graphOpen, setGraphOpen] = useState(
      typeof window === "undefined" || window.innerWidth >= 950,
    );
  const [register, setRegister] = useState(false);
  const [create, setCreate] = useState(false),
    [newName, setNewName] = useState(draft.newName),
    [newGoal, setNewGoal] = useState(draft.newGoal);
  const [notice, setNotice] = useState("");
  const reader = useRef<HTMLElement>(null),
    workbench = useRef<HTMLElement>(null);
  useEffect(() => {
    if (!overlay) return;
    const previous = document.activeElement as HTMLElement | null;
    reader.current?.querySelector<HTMLElement>("button")?.focus();
    return () => previous?.focus?.();
  }, [overlay]);
  const [selectedVersion, setSelectedVersion] = useState(
    draft.selectedVersions.get(projectId) ?? "",
  );
  const project = projects.find((value) => value.id === projectId),
    point =
      project?.checkpoints.find((value) => value.id === selectedVersion) ??
      project?.checkpoints.at(-1);
  const selectedPreset = engine?.aiPresets.presets.find(
    (value) => value.id === engine.aiPresets.selectedId,
  );
  const localKey = threadId || `new:${projectId}`,
    active = (engine?.jobs ?? []).filter((job) =>
      ["queued", "running"].includes(job.status),
    ).length;
  useEffect(() => {
    Object.assign(draft, {
      agentProject: projectId,
      agentThread: threadId,
      agentMode: mode,
      newName,
      newGoal,
    });
  }, [projectId, threadId, mode, newName, newGoal]);
  async function refresh(current = scope.readTicket("index")) {
    const [list, status, records] = await Promise.all([
      call<{ projects: MakerProject[] }>(api, "projects_list"),
      call<EngineStatus>(api, "status"),
      call<ThreadList>(api, "maker_threads"),
    ]);
    if (!current()) return;
    setProjects(list.projects);
    setEngine(status);
    setThreads(records.threads);
    setPendingJobs(new Set(records.pendingJobs));
    if (records.errors.length)
      setNotice(t("agent.retainedThreads", { count: records.errors.length }));
  }
  useEffect(() => {
    const current = scope.readTicket("index");
    if (native)
      void refresh(current).catch((e) => {
        if (current()) scope.setError(serviceError(e));
      });
  }, [scope.scope]);
  useEffect(() => {
    if (!native) return;
    let timer: ReturnType<typeof setTimeout>,
      live = true;
    async function poll() {
      const current = scope.readTicket("index");
      try {
        await refresh(current);
      } catch (e) {
        if (current()) scope.setError(serviceError(e));
      }
      if (live && scope.current()) timer = setTimeout(poll, 3500);
    }
    timer = setTimeout(poll, 3500);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [scope.scope, native]);
  useEffect(() => {
    setThread(null);
    setActivity([]);
    setAgentRunning(false);
    setOverlay(null);
    setPrompt(draft.prompts.get(localKey) ?? "");
    setHints(draft.agentScopes.get(localKey) ?? []);
    const current = scope.readTicket("thread");
    if (native && threadId)
      void call<MakerThread>(api, "maker_thread_read", { threadId })
        .then((value) => {
          if (!current()) return;
          if (value.id !== threadId)
            throw new Error(t("maker.responseMismatch"));
          setThread(value);
          if (!draft.agentScopes.has(localKey)) {
            const saved = value.rounds.at(-1)?.scopes ?? [];
            setHints(saved);
            draft.agentScopes.set(localKey, saved);
          }
        })
        .catch((e) => {
          if (current()) scope.setError(serviceError(e));
        });
  }, [threadId, projectId, scope.scope]);
  function navigate(id: string, selected = "") {
    scope.change(() => {
      setProjectId(id);
      setThreadId(selected);
      setThread(null);
      setActivity([]);
      setAgentRunning(false);
      setOverlay(null);
      const key = selected || `new:${id}`;
      setPrompt(draft.prompts.get(key) ?? "");
      setHints(draft.agentScopes.get(key) ?? []);
      setLibrary(false);
      setSelectedVersion(draft.selectedVersions.get(id) ?? "");
      setNotice("");
    });
  }
  function editPrompt(value: string) {
    setPrompt(value);
    draft.prompts.set(localKey, value);
  }
  function addScope(value: MakerScopeHint) {
    const next = [...hints.filter((hint) => hint.id !== value.id), value].slice(
      -16,
    );
    setHints(next);
    draft.agentScopes.set(localKey, next);
  }
  async function committed() {
    const current = scope.readTicket("index");
    const [records, list] = await Promise.all([
      call<ThreadList>(api, "maker_threads"),
      call<{ projects: MakerProject[] }>(api, "projects_list"),
    ]);
    if (!current()) return;
    setThreads(records.threads);
    setProjects(list.projects);
    setPendingJobs(new Set(records.pendingJobs));
    const recorded = records.threads.find((value) => value.id === threadId);
    if (recorded?.projectId && recorded.projectId !== projectId) {
      setProjectId(recorded.projectId);
      draft.agentProject = recorded.projectId;
    }
    setNotice(t("agent.archiveSuccess"));
  }
  function onPending(jobId: string, pending: boolean) {
    setPendingJobs((old) => {
      const next = new Set(old);
      pending ? next.add(jobId) : next.delete(jobId);
      return next;
    });
  }
  function source(
    jobId = point?.jobId,
    operationId = point?.operationId,
    path?: string,
  ) {
    if (jobId && operationId)
      setOverlay({ type: "source", jobId, operationId, path });
  }
  function handleActivity(next: MakerActivity[], running: boolean) {
    setActivity((old) => [
      ...next,
      ...old.filter(
        (value) =>
          value.name.startsWith("review.") &&
          !next.some((item) => item.name === value.name),
      ),
    ]);
    setAgentRunning(running);
  }
  async function send(fresh = false) {
    const request = scopePrompt(prompt, hints),
      capturedKey = localKey;
    const value = await call<MakerThread>(api, "maker_thread_run", {
      ...(threadId && !fresh ? { threadId } : {}),
      ...(project && point
        ? {
            id: project.id,
            expectedRevision: project.revision,
            checkpointId: point.id,
          }
        : { name: thread?.name ?? newName, notes: thread?.notes ?? newGoal }),
      prompt: request,
      request: prompt,
      scopes: hints,
      mode: command === "plan" ? "plan" : mode,
    });
    if (!scope.responseCurrent()) return;
    if (draft.prompts.get(capturedKey) === prompt) {
      draft.prompts.delete(capturedKey);
      setPrompt("");
    }
    draft.agentScopes.set(value.id, hints);
    setThreadId(value.id);
    setThread(value);
    setLibrary(false);
    setNotice("");
    await refresh();
  }
  const visible = threads.filter((value) =>
    projectId ? value.projectId === projectId : value.id === threadId,
  );
  const filtered = projects.filter((value) =>
    value.name.toLowerCase().includes(query.toLowerCase()),
  );
  const groups = ["running", "review", "completed"] as const;
  const blocked =
    scope.busy ||
    !native ||
    !selectedPreset ||
    active >= 2 ||
    !!project?.archived ||
    agentRunning ||
    point?.workflow === "porter" ||
    !!(thread?.rounds.at(-1) && pendingJobs.has(thread.rounds.at(-1)!.jobId));
  const command = prompt.match(/^\/(plan|review|port)(?:\s|$)/)?.[1];
  function submit() {
    if (command === "review") {
      workbench.current?.querySelector(".ma-change-card")?.scrollIntoView?.({
        block: "start",
        behavior: window.matchMedia?.("(prefers-reduced-motion: reduce)")
          .matches
          ? "instant"
          : "smooth",
      });
      setNotice(t("agent.reviewCommand"));
      return;
    }
    if (command === "port") {
      setNotice(t("agent.portCommand"));
      return;
    }
    if (blocked || !prompt.trim()) return;
    void scope.perform(() => send());
  }
  return (
    <section
      ref={workbench}
      className="maker-agent"
      aria-label={t("experimental.maker")}
    >
      {library ? (
        <div className="ma-library">
          <header className="ma-library-header">
            <div>
              <span className="ma-eyebrow">PCL RH · Agent Workspace</span>
              <h2>{t("experimental.maker")}</h2>
              <p>{t("agent.libraryIntro")}</p>
            </div>
            <button
              className="ce-button ma-pill ma-execute"
              disabled={!native}
              onClick={() => setCreate(true)}
            >
              <Plus size={16} />
              {t("maker.newProject")}
            </button>
          </header>
          <div className="ma-library-bar">
            <button
              className="ce-button"
              disabled={!native}
              onClick={() => setRegister(true)}
            >
              {t("maker.registerExisting")}
            </button>
            <div className="ma-search">
              <Search size={15} />
              <input
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setLibraryPage(0);
                }}
                placeholder={t("maker.searchProjects")}
                aria-label={t("maker.searchProjects")}
              />
            </div>
            <button className="ma-text-button" onClick={onConfigureAi}>
              <Sparkles size={14} />
              {selectedPreset?.name ?? t("agent.configureAi")}
            </button>
          </div>
          <div className="ma-project-grid">
            {filtered
              .slice(libraryPage * 12, libraryPage * 12 + 12)
              .map((value) => (
                <button
                  className="ma-project-card"
                  key={value.id}
                  disabled={!native}
                  onClick={() => navigate(value.id)}
                >
                  <Layers size={23} />
                  <h3>{value.name}</h3>
                  <small>
                    {t("agent.versionsCount", {
                      count: value.checkpoints.length,
                    })}{" "}
                    · {formatDate(value.updatedAt)}
                  </small>
                  {value.archived && (
                    <span className="ma-badge">
                      {t("agent.archivedProject")}
                    </span>
                  )}
                  <span className="ma-project-enter">
                    {t("agent.enterProject")} →
                  </span>
                </button>
              ))}
          </div>
          {!filtered.length && (
            <div className="ma-library-empty">
              <Sparkles size={32} />
              <h3>{t("agent.emptyLibrary")}</h3>
              <p>{t("agent.emptyLibraryHint")}</p>
              <button
                className="ce-button ma-pill ma-execute"
                disabled={!native}
                onClick={() => setCreate(true)}
              >
                {t("maker.newProject")}
              </button>
            </div>
          )}
          <div className="ma-page">
            <button
              disabled={!libraryPage}
              onClick={() => setLibraryPage(libraryPage - 1)}
            >
              ‹
            </button>
            <span>{filtered.length}</span>
            <button
              disabled={(libraryPage + 1) * 12 >= filtered.length}
              onClick={() => setLibraryPage(libraryPage + 1)}
            >
              ›
            </button>
          </div>
          {threads.some((value) => value.projectId === null) && (
            <section className="ma-unregistered">
              <h3>{t("agent.initialThreads")}</h3>
              {threads
                .filter((value) => value.projectId === null)
                .map((value) => (
                  <button key={value.id} onClick={() => navigate("", value.id)}>
                    <MessageSquare size={14} />
                    <span>{value.name}</span>
                    <small>{value.title}</small>
                  </button>
                ))}
            </section>
          )}
        </div>
      ) : (
        <>
          <div className={`ma-workbench ${graphOpen ? "" : "graph-closed"}`}>
            <aside className="ma-thread-sidebar">
              <header className="ma-column-header">
                <button
                  className="ma-icon"
                  aria-label={t("agent.projectLibrary")}
                  onClick={() => scope.change(() => setLibrary(true))}
                >
                  <ArrowLeft size={16} />
                </button>
                <CeSelect
                  aria-label={t("agent.project")}
                  value={projectId || "new"}
                  onChange={(e) => navigate(e.target.value)}
                >
                  {!project && (
                    <option value="new">
                      {thread?.name ?? newName ?? t("maker.newProject")}
                    </option>
                  )}
                  {projects.map((value) => (
                    <option key={value.id} value={value.id}>
                      {value.name}
                    </option>
                  ))}
                </CeSelect>
              </header>
              <div className="ma-thread-scroll">
                {groups.map((group) => {
                  const entries = visible.filter(
                    (value) =>
                      threadGroup(value, engine?.jobs ?? [], pendingJobs) ===
                      group,
                  );
                  return (
                    <section className="ma-thread-group" key={group}>
                      <h3>
                        {group === "running" ? (
                          <Circle size={10} />
                        ) : group === "review" ? (
                          <Diamond size={10} />
                        ) : (
                          <Check size={12} />
                        )}
                        <span>{t(`agent.group.${group}`)}</span>
                        <small>{entries.length}</small>
                      </h3>
                      {entries.map((value) => (
                        <button
                          key={value.id}
                          className={`ma-thread-entry ${threadId === value.id ? "selected" : ""}`}
                          onClick={() =>
                            navigate(value.projectId ?? "", value.id)
                          }
                        >
                          <span>{value.title}</span>
                          <small>
                            {formatDate(value.updatedAt, {
                              month: "short",
                              day: "numeric",
                              hour: "2-digit",
                              minute: "2-digit",
                            })}
                          </small>
                        </button>
                      ))}
                    </section>
                  );
                })}
                <button
                  className="ma-new-thread"
                  disabled={!project}
                  onClick={() => navigate(projectId)}
                >
                  <Plus size={15} />
                  {t("agent.newThread")}
                </button>
              </div>
              <footer className="ma-thread-footer">
                <button
                  disabled={!project}
                  onClick={() => setOverlay({ type: "charter" })}
                >
                  <BookOpen size={15} />
                  {t("agent.charter")}
                </button>
                <button
                  disabled={!project}
                  onClick={() => setOverlay({ type: "versions" })}
                >
                  <GitBranch size={15} />
                  {t("agent.versions")}
                </button>
                <button onClick={onConfigureAi}>
                  <Settings2 size={14} />
                  <span>{selectedPreset?.name ?? t("agent.configureAi")}</span>
                </button>
              </footer>
            </aside>
            <main className="ma-thread-main">
              <header className="ma-column-header">
                <div>
                  <MessageSquare size={16} />
                  <strong>{thread?.title ?? t("agent.newThread")}</strong>
                </div>
                <button
                  className="ma-icon"
                  aria-label={t("agent.toggleGraph")}
                  aria-pressed={graphOpen}
                  onClick={() => setGraphOpen(!graphOpen)}
                >
                  <Layers size={16} />
                </button>
              </header>
              {thread &&
                project &&
                pendingJobs.has(thread.rounds.at(-1)?.jobId ?? "") &&
                thread.rounds.at(-1)?.checkpointId !==
                  project.checkpoints.at(-1)?.id && (
                  <p className="ma-base-warning">{t("agent.branchWarning")}</p>
                )}
              {thread ? (
                <MakerThreadView
                  api={api}
                  native={native}
                  thread={thread}
                  recordedSources={
                    project?.checkpoints.map(
                      (value) => `${value.jobId}:${value.operationId}`,
                    ) ?? []
                  }
                  units={hints}
                  onDiff={(title, text) =>
                    setOverlay({ type: "diff", title, text })
                  }
                  onFollowup={(text) => editPrompt(text)}
                  onCommitted={committed}
                  onPending={onPending}
                  onActivity={handleActivity}
                  onSource={source}
                />
              ) : (
                <div className="ma-thread-welcome">
                  <span className="ma-welcome-shape">
                    <Sparkles size={28} />
                  </span>
                  <h2>{t("agent.threadWelcome")}</h2>
                  <p>{t("agent.threadWelcomeHint")}</p>
                  <div className="ma-start-ideas">
                    {(["ask", "plan", "execute"] as const).map((value) => (
                      <button
                        key={value}
                        onClick={() => {
                          setMode(value);
                          editPrompt(t(`agent.example.${value}`));
                        }}
                      >
                        <span>{t(`agent.mode.${value}`)}</span>
                        {t(`agent.example.${value}`)}
                      </button>
                    ))}
                  </div>
                </div>
              )}
              <form
                className="ma-composer"
                onSubmit={(e) => {
                  e.preventDefault();
                  submit();
                }}
                onDragOver={(e) => {
                  if (
                    e.dataTransfer.types.includes(
                      "application/x-pcl-maker-unit",
                    )
                  )
                    e.preventDefault();
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  try {
                    const value = JSON.parse(
                      e.dataTransfer.getData("application/x-pcl-maker-unit"),
                    ) as MakerScopeHint;
                    if (
                      typeof value.id === "string" &&
                      typeof value.name === "string" &&
                      Array.isArray(value.files) &&
                      typeof value.notes === "string"
                    )
                      addScope(value);
                  } catch {
                    setNotice(t("agent.invalidScope"));
                  }
                }}
              >
                {hints.length > 0 && (
                  <div className="ma-scope-chips">
                    {hints.map((hint) => (
                      <button
                        type="button"
                        key={hint.id}
                        title={hint.files.join("\n")}
                        onClick={() => {
                          const next = hints.filter(
                            (value) => value.id !== hint.id,
                          );
                          setHints(next);
                          draft.agentScopes.set(localKey, next);
                        }}
                      >
                        @{t(`maker.kind.${hint.kind}`)}/{hint.name}
                        <X size={11} />
                      </button>
                    ))}
                  </div>
                )}
                <textarea
                  value={prompt}
                  maxLength={12000}
                  aria-label={t("agent.instruction")}
                  placeholder={t("agent.composerPlaceholder")}
                  onChange={(e) => {
                    editPrompt(e.target.value);
                    if (/^\/plan(?:\s|$)/.test(e.target.value)) setMode("plan");
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                      e.preventDefault();
                      submit();
                    }
                  }}
                />
                {project && prompt.match(/@([^\n@]*)$/) && (
                  <MakerMentions
                    api={api}
                    native={native}
                    project={project}
                    query={prompt.match(/@([^\n@]*)$/)![1]}
                    onChoose={(value) => {
                      addScope(value);
                      editPrompt(prompt.replace(/@([^\n@]*)$/, ""));
                    }}
                  />
                )}
                {prompt.startsWith("/") && !command && (
                  <div className="ma-command-menu">
                    {["plan", "review", "port"].map((value) => (
                      <button
                        type="button"
                        key={value}
                        onClick={() => {
                          editPrompt(`/${value} `);
                          if (value === "plan") setMode("plan");
                        }}
                      >
                        /{value}
                      </button>
                    ))}
                  </div>
                )}
                <div className="ma-composer-bottom">
                  <div
                    className="ma-modes"
                    role="group"
                    aria-label={t("agent.permissionMode")}
                  >
                    {(["ask", "plan", "execute"] as const).map((value) => (
                      <button
                        type="button"
                        aria-pressed={mode === value}
                        key={value}
                        onClick={() => setMode(value)}
                      >
                        {t(`agent.mode.${value}`)}
                      </button>
                    ))}
                  </div>
                  <small>
                    {point
                      ? `v${project!.checkpoints.indexOf(point) + 1}`
                      : t("agent.initialSource")}
                  </small>
                  <button
                    className="ma-send ma-pill"
                    type="submit"
                    disabled={blocked || !prompt.trim()}
                    aria-label={t("agent.send")}
                  >
                    <ArrowUp size={17} />
                  </button>
                </div>
                {!selectedPreset && (
                  <button
                    className="ma-text-button"
                    type="button"
                    onClick={onConfigureAi}
                  >
                    {t("agent.configureAi")}
                  </button>
                )}
              </form>
            </main>
            {graphOpen &&
              (project ? (
                <MakerGraph
                  key={`${project.id}:${project.revision}:${graphEpoch}`}
                  api={api}
                  native={native}
                  project={project}
                  point={point!}
                  activity={activity}
                  running={agentRunning}
                  changed={
                    activity.find((value) => value.name === "review.changed")
                      ?.steps ?? []
                  }
                  onScope={addScope}
                  onSource={(path) =>
                    source(point?.jobId, point?.operationId, path)
                  }
                  onEdit={(unit, kind) =>
                    setOverlay({
                      type: "unit",
                      unitId: unit?.id ?? "new",
                      kind: unit?.kind ?? kind ?? "items",
                    })
                  }
                />
              ) : (
                <aside className="ma-graph ma-empty-graph">
                  <header className="ma-column-header">
                    <Layers size={16} />
                    <strong>{t("agent.graph")}</strong>
                  </header>
                  <p>{t("agent.graphAfterSource")}</p>
                </aside>
              ))}
          </div>
          <footer className="ma-statusbar">
            <span>
              {point
                ? t("agent.sourceVersion", {
                    version: project!.checkpoints.indexOf(point) + 1,
                  })
                : t("agent.noSource")}
            </span>
            <span>{t("agent.compileNotRun")}</span>
            <span>{t("agent.gameNotRun")}</span>
            <span className="ma-engine">
              <i className={agentRunning ? "ma-morph small" : ""} />
              {t("agent.piEngine")} ·{" "}
              {engine?.liveAvailable
                ? t("agent.ready")
                : t("agent.unconfigured")}
            </span>
          </footer>
        </>
      )}
      {notice && (
        <p className="ma-notice" role="status">
          {notice}
        </p>
      )}
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
      <ExperimentalVersion version="1.1-preview" />
      {register && (
        <MakerRegisterProject
          api={api}
          native={native}
          jobs={engine?.jobs ?? []}
          onClose={() => setRegister(false)}
          onRecorded={async (value) => {
            setRegister(false);
            await refresh();
            navigate(value.id);
          }}
        />
      )}
      {create && (
        <InstanceOperationDialog
          title={t("maker.newProject")}
          titleId="ma-create-title"
          busy={false}
          committing={false}
          confirmLabel={t("agent.startProject")}
          confirmDisabled={!native || !newName.trim()}
          onClose={() => setCreate(false)}
          onConfirm={() => {
            setCreate(false);
            setProjectId("");
            setThreadId("");
            setThread(null);
            setLibrary(false);
            setPrompt(newGoal);
            draft.prompts.set("new:", newGoal);
            setHints([]);
          }}
        >
          <label>
            {t("experimental.projectName")}
            <input
              className="ce-field"
              value={newName}
              maxLength={120}
              onChange={(e) => setNewName(e.target.value)}
            />
          </label>
          <label>
            {t("agent.initialCharter")}
            <textarea
              className="ce-field"
              rows={5}
              value={newGoal}
              maxLength={8192}
              onChange={(e) => setNewGoal(e.target.value)}
            />
          </label>
          <p>{t("agent.projectSourceRequirement")}</p>
        </InstanceOperationDialog>
      )}
      {overlay && (
        <div
          className="ma-overlay"
          onKeyDown={(e) => {
            if (e.key === "Escape" && !scope.busy) setOverlay(null);
          }}
        >
          <div
            className="ma-overlay-scrim"
            onClick={() => {
              if (!scope.busy) setOverlay(null);
            }}
          />
          <section
            ref={reader}
            className="ma-reader"
            onKeyDown={(e) => {
              if (e.key !== "Tab") return;
              const targets = [
                ...e.currentTarget.querySelectorAll<HTMLElement>(
                  'button:not(:disabled),input:not(:disabled),textarea:not(:disabled),[tabindex="0"]',
                ),
              ].filter((node) => node.offsetParent !== null);
              const first = targets[0],
                last = targets.at(-1);
              if (e.shiftKey && document.activeElement === first) {
                e.preventDefault();
                last?.focus();
              } else if (!e.shiftKey && document.activeElement === last) {
                e.preventDefault();
                first?.focus();
              }
            }}
            role="dialog"
            aria-modal="true"
            aria-label={
              overlay.type === "diff"
                ? t("agent.viewDiff")
                : overlay.type === "source"
                  ? t("agent.openSource")
                  : t(
                      `agent.${overlay.type === "unit" ? "dossier" : overlay.type}`,
                    )
            }
          >
            <header className="ma-column-header">
              <strong>
                {overlay.type === "diff"
                  ? overlay.title
                  : overlay.type === "source"
                    ? t("agent.openSource")
                    : t(
                        `agent.${overlay.type === "unit" ? "dossier" : overlay.type}`,
                      )}
              </strong>
              <button
                className="ma-icon"
                disabled={scope.busy}
                aria-label={t("agent.close")}
                onClick={() => setOverlay(null)}
              >
                <X size={18} />
              </button>
            </header>
            <div className="ma-reader-body">
              {overlay.type === "diff" && (
                <pre className="ma-diff-content">{overlay.text}</pre>
              )}
              {overlay.type === "source" && (
                <MakerSource
                  api={api}
                  native={native}
                  draft={draft}
                  readOnly
                  initialPath={overlay.path}
                  source={overlay}
                  project={
                    project?.checkpoints.some(
                      (value) =>
                        value.jobId === overlay.jobId &&
                        value.operationId === overlay.operationId,
                    )
                      ? project
                      : undefined
                  }
                  point={project?.checkpoints.find(
                    (value) =>
                      value.jobId === overlay.jobId &&
                      value.operationId === overlay.operationId,
                  )}
                />
              )}
              {overlay.type === "unit" && project && (
                <MakerFeatures
                  api={api}
                  native={native}
                  project={project}
                  draft={draft}
                  route={{
                    projectId: project.id,
                    section: "unit",
                    kind: overlay.kind,
                    unitId: overlay.unitId,
                  }}
                  onContext={() => {}}
                  onSaved={(unit) => {
                    setGraphEpoch((value) => value + 1);
                    if (hints.some((hint) => hint.id === unit.id))
                      addScope(unit);
                  }}
                  onRoute={() => {
                    setOverlay(null);
                    void refresh();
                  }}
                />
              )}
              {overlay.type === "charter" && project && (
                <MakerCharter
                  api={api}
                  native={native}
                  project={project}
                  draft={draft}
                  onSaved={async () => {
                    await refresh();
                    setNotice(t("agent.charterSaved"));
                  }}
                />
              )}
              {overlay.type === "versions" && project && point && (
                <MakerVersions
                  api={api}
                  native={native}
                  project={project}
                  point={point}
                  onBase={(id) => {
                    setSelectedVersion(id);
                    draft.selectedVersions.set(project.id, id);
                  }}
                  onBranch={(id) => {
                    draft.selectedVersions.set(project.id, id);
                    navigate(project.id);
                    setSelectedVersion(id);
                    setOverlay(null);
                  }}
                  onSource={source}
                />
              )}
            </div>
          </section>
        </div>
      )}
    </section>
  );
}
