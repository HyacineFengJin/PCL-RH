/** The workspace controller is the project service, not a view. Mutations are
 * queued against the latest native revision; layout disposal never deletes a
 * conversation, cancels a job or discards a typed buffer. */
import { createContext, useContext, useEffect, useRef, useState } from "react";
import type { Api } from "../types";
import { experimentalCall, type JobView } from "../experimentalTypes";
import type {
  Area,
  FileBuffer,
  Layout,
  Project,
  Snapshot,
  Mode,
  JsonValue,
} from "./types";
import { area, depth, leaves, map, preset, replace, split } from "./layout";
import { label } from "./locale";
import {
  createControllerLifecycle,
  type LifecycleState,
} from "./controllerLifecycle";
type DirtyDraft = {
  projectId: string;
  conversationId: string;
  value: string;
  ticket: number;
  mode?: Mode;
};
type SourceBuffer = { projectId: string; path: string; value: FileBuffer };
// Only the tiny selection locator survives a controller lifetime. Large state
// belongs to the mounted service and is released after successful suspension.
const lastProject = new Map<string, string>();
export function useController(hostApi: Api, native: boolean) {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null),
    [projectId, setProjectId] = useState(lastProject.get("selected") ?? ""),
    [layout, setLayout] = useState<Layout>(preset("conversation")),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(0),
    [epoch, update] = useState(0),
    [jobs, setJobs] = useState<Record<string, JobView>>({}),
    [filesEpoch, setFilesEpoch] = useState(0);
  const latest = useRef<Snapshot | null>(null),
    serial = useRef(Promise.resolve()),
    revision = useRef<string | null>(null),
    alive = useRef(true),
    loadSequence = useRef(0),
    settling = useRef(new Set<string>()),
    draftTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const draftsRef = useRef(new Map<string, string>()),
    buffersRef = useRef(new Map<string, SourceBuffer>()),
    dirtyDrafts = useRef(new Map<string, DirtyDraft>()),
    draftChange = useRef(0),
    lifecycleRef = useRef(createControllerLifecycle()),
    failures = useRef(new Set<string>()),
    jobsRef = useRef<Record<string, JobView>>({}),
    suspendTask = useRef<Promise<boolean> | null>(null),
    apiRef = useRef(hostApi);
  apiRef.current = hostApi;
  const drafts = draftsRef.current,
    buffers = buffersRef.current,
    lifecycle = lifecycleRef.current;
  const usable = () => alive.current && !lifecycle.suspended;
  const assertUsable = () => {
    if (!usable()) throw new Error("RH Code workspace is suspended");
  };
  const failureKey = (operation: string, args: Record<string, unknown>) =>
    JSON.stringify([
      operation,
      args.projectId,
      args.conversationId,
      args.path,
      args.id,
    ]);
  const release = () => {
    for (const timer of draftTimers.current.values()) clearTimeout(timer);
    draftTimers.current.clear();
    drafts.clear();
    buffers.clear();
    dirtyDrafts.current.clear();
    pendingLayouts.current.clear();
    failures.current.clear();
    latest.current = null;
    revision.current = null;
    jobsRef.current = {};
    settling.current.clear();
    currentLayout.current = preset("conversation");
  };
  const releaseAfterUnmount = () => {
    if (!alive.current && !Object.values(lifecycle.pending).some(Boolean))
      release();
  };
  // Stable identity avoids restarting view read effects for every source edit.
  // Requests issued by views also pin the service until their callbacks finish.
  const publicApi = useRef<Api>(
    async <T,>(command: string, args?: Record<string, unknown>) => {
      assertUsable();
      const operation = String(args?.operation ?? command);
      const readOnly =
        /(?:_read|_list|_files|_units|_search|_compare|_compare_file)$/.test(
          operation,
        ) ||
        [
          "bootstrap",
          "files",
          "status",
          "catalog",
          "maker_threads",
          "extensions_list",
        ].includes(operation);
      if (!readOnly) lifecycle.edited();
      const end = lifecycle.begin("external", !readOnly),
        failure = "external:" + operation;
      try {
        const value = await apiRef.current<T>(command, args);
        if (usable()) failures.current.delete(failure);
        return value;
      } catch (e) {
        if (usable() && !readOnly) failures.current.add(failure);
        throw e;
      } finally {
        end();
        releaseAfterUnmount();
      }
    },
  ).current;
  const api = hostApi;
  const selection = useRef(projectId);
  const layoutChange = useRef(0);
  const pendingLayouts = useRef(
    new Map<string, { ticket: number; layout: Layout }>(),
  );
  const currentLayout = useRef(layout);
  currentLayout.current = layout;
  selection.current = projectId;
  const showLayout = (next: Layout) => {
    currentLayout.current = next;
    setLayout(next);
  };
  const projectLayout = (p: Project) =>
    pendingLayouts.current.get(p.id)?.layout ??
    p.layout ??
    preset(
      "conversation",
      p.conversations.map((c) => c.id),
    );
  const setData = (data: Snapshot) => {
    if (!usable()) return data;
    for (const p of data.projects)
      for (const c of p.conversations) {
        const key = p.id + ":" + c.id;
        if (!drafts.has(key)) drafts.set(key, c.draft ?? "");
      }
    latest.current = data;
    revision.current = data.revision;
    if (alive.current) setSnapshot(data);
    return data;
  };
  const read = async () => {
    assertUsable();
    const end = lifecycle.begin("read"),
      ticket = ++loadSequence.current;
    try {
      const data = await api<Snapshot>("rh_code_call", {
        operation: "bootstrap",
        args: {},
      });
      if (usable() && ticket === loadSequence.current) {
        setData(data);
        const owned = data.projects.find((p) => p.id === selection.current);
        if (owned) showLayout(projectLayout(owned));
      }
      return data;
    } finally {
      end();
      releaseAfterUnmount();
    }
  };
  useEffect(() => {
    alive.current = true;
    return () => {
      // Accepted drafts enter the queue before admission closes. Late replies
      // may finish persistence but cannot refill caches or update a new service.
      for (const id of draftTimers.current.keys())
        void persistDraft(id).catch(() => {});
      alive.current = false;
      loadSequence.current++;
      releaseAfterUnmount();
    };
  }, []);
  useEffect(() => {
    if (native)
      void read().catch((e) => {
        if (usable()) setError(String(e));
      });
  }, [hostApi, native]);
  const project = snapshot?.projects.find((p) => p.id === projectId) ?? null;
  const choose = (p: Project) => {
    if (!usable()) return;
    lifecycle.touch();
    selection.current = p.id;
    setProjectId(p.id);
    lastProject.set("selected", p.id);
    showLayout(
      projectLayout(latest.current?.projects.find((q) => q.id === p.id) ?? p),
    );
    setError("");
  };
  function write(operation: string, args: Record<string, unknown> = {}) {
    if (!usable())
      return Promise.reject(new Error("RH Code workspace is suspended"));
    const end = lifecycle.begin("mutation"),
      failure = failureKey(operation, args);
    setBusy((v) => v + 1);
    const task = serial.current
      .catch(() => {})
      .then(async () => {
        try {
          loadSequence.current++;
          const expectedRevision = revision.current ?? (await read()).revision;
          const data = await api<Snapshot>("rh_code_call", {
            operation,
            args: { ...args, expectedRevision },
          });
          // Accepted writes can finish after unmount. Their next writer needs
          // only the revision; snapshots cannot refill the unloaded service.
          revision.current = data.revision;
          setData(data);
          failures.current.delete(failure);
          if (usable()) setError("");
          return data;
        } catch (e) {
          if (usable()) {
            failures.current.add(failure);
            setError(String(e));
          }
          throw e;
        } finally {
          end();
          if (usable()) setBusy((v) => v - 1);
          releaseAfterUnmount();
        }
      });
    serial.current = task.then(
      () => {},
      () => {},
    );
    return task;
  }
  const mutateLayout = (next: Layout) => {
    const owner = project;
    // View callbacks may finish after navigation. Their captured project cannot
    // write the newly selected project's presentation back to the old project.
    if (!owner || selection.current !== owner.id || !usable()) return;
    if (depth(next) > 7 || leaves(next).length > 12) {
      setError(
        label(
          "区域过多，请先关闭或合并一些区域",
          "Close some areas before splitting this layout further",
        ),
      );
      return;
    }
    lifecycle.edited();
    const ticket = ++layoutChange.current;
    pendingLayouts.current.set(owner.id, { ticket, layout: next });
    showLayout(next);
    const settle = () => {
      if (pendingLayouts.current.get(owner.id)?.ticket !== ticket) return;
      pendingLayouts.current.delete(owner.id);
      // Pending geometry survives project navigation and failed writes. Only
      // the newest successful acknowledgement settles the local layout.
      if (alive.current && selection.current === owner.id)
        showLayout(
          projectLayout(
            latest.current?.projects.find((p) => p.id === owner.id) ?? owner,
          ),
        );
    };
    void write("layout_save", { projectId: owner.id, layout: next }).then(
      settle,
      () => {},
    );
  };
  const patchArea = (id: string, patch: Partial<Area>) => {
    if (!leaves(currentLayout.current).some((n) => n.id === id)) return;
    mutateLayout(
      map(currentLayout.current, (n) => (n.id === id ? { ...n, ...patch } : n)),
    );
  };
  const updateAreaState = (
    id: string,
    view: string,
    state: Record<string, JsonValue>,
  ) => {
    // A late callback from an unloaded view cannot update the replacement view.
    const current = leaves(currentLayout.current).find((n) => n.id === id);
    if (!current || current.view !== view) return;
    patchArea(id, { state: { ...current.state, [view]: state } });
  };
  async function newConversation(areaId?: string) {
    if (!project) return;
    const data = await write("conversation_create", {
      projectId: project.id,
      title:
        label("新对话", "New conversation") +
        " " +
        (project.conversations.length + 1),
    });
    const conversation = data.projects
      .find((p) => p.id === project.id)!
      .conversations.at(-1)!;
    if (areaId && selection.current === project.id)
      patchArea(areaId, { view: "chat", conversation: conversation.id });
    return conversation.id;
  }
  const openConversation = (id: string) => {
    const chat = leaves(layout).find((n) => n.view === "chat");
    if (chat) patchArea(chat.id, { conversation: id });
    else mutateLayout(split("row", layout, area("chat", id), 0.6));
  };
  const openFile = (file: string) => {
    const editor = leaves(layout).find((n) => n.view === "source");
    if (editor) patchArea(editor.id, { file });
    else {
      const pane =
        leaves(layout).find((n) => n.view === "files") ?? leaves(layout)[0];
      mutateLayout(
        replace(
          layout,
          pane.id,
          split("column", pane, { ...area("source"), file }, 0.4),
        ),
      );
    }
  };
  const key = (id: string) => projectId + ":" + id;
  const draft = (id: string) => drafts.get(key(id)) ?? "";
  function persistDraft(id: string) {
    const timer = draftTimers.current.get(id);
    if (timer) clearTimeout(timer);
    draftTimers.current.delete(id);
    const pending = dirtyDrafts.current.get(id);
    if (!pending) return Promise.resolve();
    return write("conversation_draft", {
      projectId: pending.projectId,
      conversationId: pending.conversationId,
      draft: pending.value,
      ...(pending.mode ? { mode: pending.mode } : {}),
    }).then(() => {
      if (dirtyDrafts.current.get(id)?.ticket === pending.ticket)
        dirtyDrafts.current.delete(id);
    });
  }
  function setOwnedDraft(ownedProject: string, id: string, value: string) {
    if (!usable()) return;
    const ownedKey = ownedProject + ":" + id;
    drafts.set(ownedKey, value);
    dirtyDrafts.current.set(ownedKey, {
      projectId: ownedProject,
      conversationId: id,
      value,
      ticket: ++draftChange.current,
    });
    lifecycle.edited();
    update((v) => v + 1);
    const existing = draftTimers.current.get(ownedKey);
    if (existing) clearTimeout(existing);
    draftTimers.current.set(
      ownedKey,
      setTimeout(() => {
        void persistDraft(ownedKey).catch(() => {});
      }, 400),
    );
  }
  const setDraft = (id: string, value: string) =>
    setOwnedDraft(projectId, id, value);
  async function loadFile(file: string) {
    if (!project || !usable()) return;
    const id = key(file),
      existing = buffers.get(id);
    if (existing) return existing.value;
    const end = lifecycle.begin("source");
    try {
      const result = await api<{ content: string; sha256: string }>(
        "rh_code_call",
        {
          operation: "file_read",
          args: { projectId: project.id, path: file },
        },
      );
      if (!usable()) return;
      const opened = buffers.get(id);
      if (opened) return opened.value;
      const value = { ...result, original: result.content };
      buffers.set(id, { projectId: project.id, path: file, value });
      update((v) => v + 1);
      return value;
    } finally {
      end();
      releaseAfterUnmount();
    }
  }
  const buffer = (file: string) => buffers.get(key(file))?.value;
  const edit = (file: string, content: string) => {
    if (!usable()) return;
    const value = buffer(file);
    if (value && value.content !== content) {
      value.content = content;
      lifecycle.edited();
      update((v) => v + 1);
    }
  };
  async function saveOwnedFile(owned: SourceBuffer) {
    assertUsable();
    const { projectId: owner, path: file, value } = owned,
      submitted = value.content,
      end = lifecycle.begin("source");
    try {
      await write("file_save", {
        projectId: owner,
        path: file,
        content: submitted,
        sha256: value.sha256,
      });
      const disk = await api<{ content: string; sha256: string }>(
        "rh_code_call",
        {
          operation: "file_read",
          args: { projectId: owner, path: file },
        },
      );
      if (!usable()) return;
      // Verification cannot acknowledge an external editor's intervening change.
      if (disk.content !== submitted)
        throw new Error(
          "Source changed on disk after saving; refresh and compare before saving",
        );
      value.original = disk.content;
      value.sha256 = disk.sha256;
      failures.current.delete("source:" + owner + ":" + file);
      // value.content may contain a newer edit typed during I/O.
      update((v) => v + 1);
      setFilesEpoch((v) => v + 1);
    } catch (e) {
      if (usable()) {
        failures.current.add("source:" + owner + ":" + file);
        setError(String(e));
      }
      throw e;
    } finally {
      end();
      releaseAfterUnmount();
    }
  }
  async function saveFile(file: string) {
    const owned = buffers.get(key(file));
    if (owned) await saveOwnedFile(owned);
  }
  async function run(
    areaId: string,
    conversationId: string,
    mode: Mode,
    request?: string,
  ) {
    const p = latest.current?.projects.find((p) => p.id === projectId),
      conversation = p?.conversations.find((c) => c.id === conversationId);
    if (!p || !conversation || !usable()) return;
    const content = request ?? draft(conversationId);
    if (!content.trim()) return;
    // Reserve the conversation in the renderer before yielding. Other views of
    // the same conversation cannot accidentally submit a second paid run.
    if (settling.current.has(conversationId) || conversation.activeJob) return;
    settling.current.add(conversationId);
    const end = lifecycle.begin("operation");
    setBusy((v) => v + 1);
    let jobId: string | undefined;
    try {
      const ownedKey = p.id + ":" + conversationId;
      if (dirtyDrafts.current.has(ownedKey)) {
        const pending = dirtyDrafts.current.get(ownedKey)!;
        dirtyDrafts.current.set(ownedKey, { ...pending, mode });
        await persistDraft(ownedKey);
      }
      await serial.current;
      assertUsable();
      const created = await experimentalCall<{ jobId: string }>(
        api,
        "rh_code_run",
        {
          projectId: p.id,
          conversationId,
          expectedRevision: latest.current!.revision,
          request: content,
          mode,
          targetIndex: 0,
        },
      );
      jobId = created.jobId;
      assertUsable();
      if (!jobId) throw new Error("Agent returned no task identity");
      await write("conversation_append", {
        projectId: p.id,
        conversationId,
        role: "user",
        content,
        jobId,
        activeJob: jobId,
        mode,
      });
      assertUsable();
      await experimentalCall(api, "rh_code_start", {
        projectId: p.id,
        conversationId,
        jobId,
      });
      // Launch belongs to its captured project; preserve any newer user draft.
      if (drafts.get(p.id + ":" + conversationId) === content)
        setOwnedDraft(p.id, conversationId, "");
    } catch (e) {
      if (jobId)
        await experimentalCall(api, "job_cancel", { jobId }).catch(() => {});
      if (alive.current) setError(String(e));
    } finally {
      settling.current.delete(conversationId);
      end();
      releaseAfterUnmount();
      if (alive.current) setBusy((v) => v - 1);
    }
    void areaId;
  }
  useEffect(() => {
    if (!native || !snapshot) return;
    const active = snapshot.projects.flatMap((p) =>
      p.conversations
        .filter((c) => c.activeJob)
        .map((c) => ({ project: p, conversation: c, jobId: c.activeJob! })),
    );
    if (!active.length) return;
    let cancelled = false,
      polling = false;
    const poll = async () => {
      if (polling || !usable()) return;
      polling = true;
      const end = lifecycle.begin("read", false);
      try {
        for (const item of active) {
          if (cancelled || !usable()) return;
          try {
            const job = await experimentalCall<JobView>(api, "job_read", {
              jobId: item.jobId,
            });
            if (cancelled || !usable()) return;
            if (job.summary.jobId !== item.jobId)
              throw new Error("Task identity mismatch");
            const previous = jobsRef.current[item.jobId];
            if (
              !previous ||
              previous.summary.revision !== job.summary.revision ||
              previous.summary.status !== job.summary.status ||
              previous.summary.updatedAt !== job.summary.updatedAt ||
              JSON.stringify(previous.activity) !== JSON.stringify(job.activity)
            )
              lifecycle.touch();
            jobsRef.current = { ...jobsRef.current, [item.jobId]: job };
            setJobs(jobsRef.current);
            if (
              !["queued", "running"].includes(job.summary.status) &&
              !settling.current.has(item.jobId)
            ) {
              settling.current.add(item.jobId);
              try {
                const result = job.result as { text?: string } | null;
                await write("conversation_complete", {
                  projectId: item.project.id,
                  conversationId: item.conversation.id,
                  role: "assistant",
                  content:
                    result?.text ||
                    job.error?.message ||
                    label("任务结束", "Task ended") +
                      " · " +
                      job.summary.status,
                  jobId: item.jobId,
                  activeJob: null,
                });
              } finally {
                settling.current.delete(item.jobId);
              }
            }
          } catch (e) {
            if (!cancelled) setError(String(e));
          }
        }
      } finally {
        polling = false;
        end();
        releaseAfterUnmount();
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), 1700);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [native, snapshot]);
  async function adoptSource(jobId: string, operationId: string) {
    assertUsable();
    const ownedProject = projectId,
      end = lifecycle.begin("operation");
    setBusy((v) => v + 1);
    try {
      await serial.current;
      assertUsable();
      await api("rh_code_adopt", {
        args: {
          projectId: ownedProject,
          jobId,
          operationId,
          expectedRevision: latest.current!.revision,
        },
      });
      await read();
      if (usable()) {
        setFilesEpoch((v) => v + 1);
        setError("");
      }
    } catch (e) {
      if (usable()) setError(String(e));
      throw e;
    } finally {
      end();
      if (usable()) setBusy((v) => v - 1);
      releaseAfterUnmount();
    }
  }
  const stop = async (id: string) => {
    assertUsable();
    const end = lifecycle.begin("operation");
    try {
      await experimentalCall(api, "job_cancel", { jobId: id });
    } finally {
      end();
      releaseAfterUnmount();
    }
  };
  const activeJobs = () => {
    const active = new Set(
      latest.current?.projects.flatMap((p) =>
        p.conversations.flatMap((c) => (c.activeJob ? [c.activeJob] : [])),
      ) ?? [],
    );
    return active.size + settling.current.size;
  };
  const dirty = () =>
    !!dirtyDrafts.current.size ||
    !!pendingLayouts.current.size ||
    !!failures.current.size ||
    [...buffers.values()].some((b) => b.value.content !== b.value.original);
  const getLifecycleState = () => lifecycle.state(dirty(), activeJobs());
  async function prepareSuspend(stillInactive: () => boolean = () => true) {
    if (!usable() || !stillInactive()) return false;
    // Only known revisioned mutations can be drained here. Other requests have
    // their own continuations and must finish before preparation starts.
    const pending = lifecycle.pending;
    if (
      activeJobs() ||
      pending.source ||
      pending.read ||
      pending.operation ||
      pending.external
    )
      return false;
    const generation = lifecycle.generation;
    try {
      for (const id of dirtyDrafts.current.keys()) await persistDraft(id);
      for (const [projectId, pendingLayout] of pendingLayouts.current) {
        await write("layout_save", { projectId, layout: pendingLayout.layout });
        if (
          pendingLayouts.current.get(projectId)?.ticket === pendingLayout.ticket
        )
          pendingLayouts.current.delete(projectId);
      }
      await serial.current;
      if (
        !usable() ||
        !stillInactive() ||
        generation !== lifecycle.generation ||
        activeJobs()
      )
        return false;
      // Closed source areas and other projects still own their open buffers.
      for (const owned of buffers.values()) {
        if (
          owned.value.content !== owned.value.original ||
          failures.current.has("source:" + owned.projectId + ":" + owned.path)
        )
          await saveOwnedFile(owned);
        if (
          !usable() ||
          !stillInactive() ||
          generation !== lifecycle.generation ||
          activeJobs()
        )
          return false;
      }
      await serial.current;
      const state = getLifecycleState();
      if (
        !usable() ||
        !stillInactive() ||
        generation !== lifecycle.generation ||
        state.busy ||
        state.dirty ||
        state.activeJobs
      )
        return false;
      // The final fence and release are synchronous. Parent retires this service
      // immediately after true; callbacks from its old views are now closed.
      lifecycle.suspend();
      release();
      setSnapshot(null);
      setJobs({});
      setLayout(currentLayout.current);
      return true;
    } catch (e) {
      if (usable()) setError(String(e));
      return false;
    }
  }
  const lifecycleCallbacks = useRef({ getLifecycleState, prepareSuspend });
  lifecycleCallbacks.current = { getLifecycleState, prepareSuspend };
  const lifecycleMethods = useRef({
    getLifecycleState: (): LifecycleState =>
      lifecycleCallbacks.current.getLifecycleState(),
    prepareSuspend: (stillInactive?: () => boolean): Promise<boolean> => {
      // Concurrent callers can have different visibility fences. Only the
      // original attempt may commit; a second caller retries after it settles.
      if (suspendTask.current) return Promise.resolve(false);
      suspendTask.current = lifecycleCallbacks.current
        .prepareSuspend(stillInactive)
        .finally(() => {
          suspendTask.current = null;
        });
      return suspendTask.current;
    },
  }).current;
  return {
    api: publicApi,
    ...lifecycleMethods,
    native,
    snapshot,
    project,
    projectId,
    layout,
    jobs,
    error,
    busy,
    epoch,
    filesEpoch,
    setError,
    read,
    write: (operation: string, args: Record<string, unknown> = {}) => {
      if (usable()) lifecycle.edited();
      return write(operation, args);
    },
    choose,
    home: () => {
      if (!usable()) return;
      lifecycle.touch();
      selection.current = "";
      setProjectId("");
      lastProject.delete("selected");
    },
    mutateLayout,
    patchArea,
    updateAreaState,
    newConversation,
    openConversation,
    openFile,
    draft,
    setDraft,
    loadFile,
    buffer,
    edit,
    saveFile,
    adoptSource,
    run,
    stop,
  };
}
export type Controller = ReturnType<typeof useController>;
export const CodeContext = createContext<Controller | null>(null);
export const useCode = () => {
  const value = useContext(CodeContext);
  if (!value) throw new Error("RH Code view requires its project service");
  return value;
};
