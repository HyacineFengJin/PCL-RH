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
} from "./types";
import { area, depth, leaves, map, preset, replace, split } from "./layout";
import { label } from "./locale";
const drafts = new Map<string, string>();
const buffers = new Map<string, FileBuffer>();
const lastProject = new Map<string, string>();
export function useController(api: Api, native: boolean) {
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
    alive = useRef(true),
    loadSequence = useRef(0),
    settling = useRef(new Set<string>()),
    draftTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const selection = useRef(projectId);
  const layoutChange = useRef(0);
  selection.current = projectId;
  const setData = (data: Snapshot) => {
    for (const p of data.projects)
      for (const c of p.conversations) {
        const key = p.id + ":" + c.id;
        if (!drafts.has(key)) drafts.set(key, c.draft ?? "");
      }
    latest.current = data;
    if (alive.current) setSnapshot(data);
    return data;
  };
  const read = async () => {
    const ticket = ++loadSequence.current;
    const data = await api<Snapshot>("rh_code_call", {
      operation: "bootstrap",
      args: {},
    });
    if (alive.current && ticket === loadSequence.current) {
      setData(data);
      const owned = data.projects.find((p) => p.id === selection.current);
      if (owned)
        setLayout(
          owned.layout ??
            preset(
              "conversation",
              owned.conversations.map((c) => c.id),
            ),
        );
    }
    return data;
  };
  useEffect(() => {
    alive.current = true;
    if (native) void read().catch((e) => setError(String(e)));
    return () => {
      alive.current = false;
      loadSequence.current++;
    };
  }, [api, native]);
  const project = snapshot?.projects.find((p) => p.id === projectId) ?? null;
  const choose = (p: Project) => {
    selection.current = p.id;
    setProjectId(p.id);
    lastProject.set("selected", p.id);
    setLayout(
      p.layout ??
        preset(
          "conversation",
          p.conversations.map((c) => c.id),
        ),
    );
    setError("");
  };
  useEffect(() => {
    const current = snapshot?.projects.find((p) => p.id === projectId);
    if (current && current.layout) setLayout(current.layout);
  }, [projectId]);
  function write(operation: string, args: Record<string, unknown> = {}) {
    const task = serial.current
      .catch(() => {})
      .then(async () => {
        setBusy((v) => v + 1);
        try {
          loadSequence.current++;
          const current = latest.current ?? (await read());
          const data = await api<Snapshot>("rh_code_call", {
            operation,
            args: { ...args, expectedRevision: current.revision },
          });
          setData(data);
          setError("");
          return data;
        } catch (e) {
          if (alive.current) setError(String(e));
          throw e;
        } finally {
          if (alive.current) setBusy((v) => v - 1);
        }
      });
    serial.current = task.then(
      () => {},
      () => {},
    );
    return task;
  }
  const mutateLayout = (next: Layout) => {
    if (depth(next) > 7 || leaves(next).length > 12) {
      setError(
        label(
          "区域过多，请先关闭或合并一些区域",
          "Close some areas before splitting this layout further",
        ),
      );
      return;
    }
    const ticket = ++layoutChange.current;
    const owner = project;
    setLayout(next);
    if (owner)
      void write("layout_save", { projectId: owner.id, layout: next }).catch(
        () => {
          // Only roll back this attempt. A newer resize or another project must
          // not be replaced by an earlier rejected disk write.
          if (
            alive.current &&
            selection.current === owner.id &&
            ticket === layoutChange.current
          )
            setLayout(
              latest.current?.projects.find((p) => p.id === owner.id)?.layout ??
                preset(
                  "conversation",
                  owner.conversations.map((c) => c.id),
                ),
            );
        },
      );
  };
  const patchArea = (id: string, patch: Partial<Area>) =>
    mutateLayout(map(layout, (n) => (n.id === id ? { ...n, ...patch } : n)));
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
    if (areaId)
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
  const setDraft = (id: string, value: string) => {
    const ownedProject = projectId,
      ownedKey = key(id);
    drafts.set(ownedKey, value);
    update((v) => v + 1);
    const existing = draftTimers.current.get(ownedKey);
    if (existing) clearTimeout(existing);
    draftTimers.current.set(
      ownedKey,
      setTimeout(() => {
        draftTimers.current.delete(ownedKey);
        void write("conversation_draft", {
          projectId: ownedProject,
          conversationId: id,
          draft: value,
        }).catch(() => {});
      }, 400),
    );
  };
  async function loadFile(file: string) {
    if (!project) return;
    const id = key(file);
    if (buffers.has(id)) return buffers.get(id);
    const result = await api<{ content: string; sha256: string }>(
      "rh_code_call",
      { operation: "file_read", args: { projectId: project.id, path: file } },
    );
    const value = { ...result, original: result.content };
    buffers.set(id, value);
    update((v) => v + 1);
    return value;
  }
  const buffer = (file: string) => buffers.get(key(file));
  const edit = (file: string, content: string) => {
    const value = buffer(file);
    if (value) {
      value.content = content;
      update((v) => v + 1);
    }
  };
  async function saveFile(file: string) {
    const value = buffer(file),
      owner = project,
      id = key(file);
    if (!value || !owner) return;
    const submitted = value.content;
    await write("file_save", {
      projectId: owner.id,
      path: file,
      content: submitted,
      sha256: value.sha256,
    });
    const disk = await api<{ content: string; sha256: string }>(
      "rh_code_call",
      { operation: "file_read", args: { projectId: owner.id, path: file } },
    );
    // A save completion updates the baseline, not edits typed while I/O was
    // pending. All source areas share this buffer and must keep those edits.
    buffers.set(id, {
      ...disk,
      original: disk.content,
      content: value.content === submitted ? disk.content : value.content,
    });
    update((v) => v + 1);
    setFilesEpoch((v) => v + 1);
  }
  async function run(
    areaId: string,
    conversationId: string,
    mode: Mode,
    request?: string,
  ) {
    const p = latest.current?.projects.find((p) => p.id === projectId),
      conversation = p?.conversations.find((c) => c.id === conversationId);
    if (!p || !conversation) return;
    const content = request ?? draft(conversationId);
    if (!content.trim()) return;
    // Reserve the conversation in the renderer before yielding. Other views of
    // the same conversation cannot accidentally submit a second paid run.
    if (settling.current.has(conversationId) || conversation.activeJob) return;
    settling.current.add(conversationId);
    setBusy((v) => v + 1);
    let jobId: string | undefined;
    try {
      const pendingDraft = draftTimers.current.get(p.id + ":" + conversationId);
      if (pendingDraft) {
        clearTimeout(pendingDraft);
        draftTimers.current.delete(p.id + ":" + conversationId);
        await write("conversation_draft", {
          projectId: p.id,
          conversationId,
          draft: content,
          mode,
        });
      }
      await serial.current;
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
      await experimentalCall(api, "rh_code_start", {
        projectId: p.id,
        conversationId,
        jobId,
      });
      setDraft(conversationId, "");
    } catch (e) {
      if (jobId)
        await experimentalCall(api, "job_cancel", { jobId }).catch(() => {});
      if (alive.current) setError(String(e));
    } finally {
      settling.current.delete(conversationId);
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
    let cancelled = false;
    const poll = async () => {
      for (const item of active) {
        try {
          const job = await experimentalCall<JobView>(api, "job_read", {
            jobId: item.jobId,
          });
          if (cancelled) return;
          if (job.summary.jobId !== item.jobId)
            throw new Error("Task identity mismatch");
          setJobs((v) => ({ ...v, [item.jobId]: job }));
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
                  label("任务结束", "Task ended") + " · " + job.summary.status,
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
    };
    void poll();
    const timer = setInterval(() => void poll(), 1700);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [native, snapshot]);
  async function adoptSource(jobId: string, operationId: string) {
    await serial.current;
    setBusy((v) => v + 1);
    try {
      await api("rh_code_adopt", {
        args: {
          projectId,
          jobId,
          operationId,
          expectedRevision: latest.current!.revision,
        },
      });
      setFilesEpoch((v) => v + 1);
      setError("");
    } catch (e) {
      setError(String(e));
      throw e;
    } finally {
      setBusy((v) => v - 1);
    }
  }
  const stop = async (id: string) => {
    await experimentalCall(api, "job_cancel", { jobId: id });
  };
  return {
    api,
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
    write,
    choose,
    home: () => {
      setProjectId("");
      lastProject.delete("selected");
    },
    mutateLayout,
    patchArea,
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
