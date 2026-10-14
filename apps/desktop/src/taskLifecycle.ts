import type { DownloadStatus, DownloadTaskView } from "./taskTypes";

export function taskIdentity(status: DownloadStatus): string {
  return (
    status.task_id ||
    `${status.root_id || status.root_path || ""}:${status.version || ""}`
  );
}
export function taskIsActive(status: DownloadStatus): boolean {
  return ["queued", "preparing", "downloading", "processing"].includes(
    status.stage,
  );
}
export function taskIsTerminal(status: DownloadStatus): boolean {
  return ["complete", "error", "cancelled"].includes(status.stage);
}
export function taskDisplayName(status: DownloadStatus): string {
  return status.display_name || status.version || "";
}
export function taskHasFloatingEntry(status: DownloadStatus): boolean {
  return taskIsActive(status) || status.stage === "error";
}
/** Global registry/recovery mutations refresh the current bootstrap projection
 * even when the writer targeted another root; file-only launcher jobs do not. */
export function taskNeedsBootstrap(
  task: DownloadStatus,
  currentRoot: string | null,
): boolean {
  // Repair publishes verified files individually. Cancellation or a later
  // failure can still leave useful files in the submitted root.
  if (task.kind === "instance_repair")
    return (
      !!currentRoot && taskIsTerminal(task) && task.root_id === currentRoot
    );
  if (
    [
      "resource_save",
      "modpack_prepare",
      "toolbox_download",
      "launcher_logs",
      "instance_export",
    ].includes(task.kind || "")
  )
    return false;
  const globalRecovery = [
    "java_install",
    "instance_rename",
    "instance_import",
    "modpack_install",
    "instance_delete",
    "instance_restore",
    "resource_download",
    "resource_update",
    "resource_update_restore",
  ].includes(task.kind || "");
  if (task.stage === "error")
    return (
      globalRecovery ||
      (task.kind === "resource_operation" && task.root_id === currentRoot)
    );
  if (task.stage !== "complete") return false;
  return (
    globalRecovery ||
    task.kind === "install" ||
    !task.root_id ||
    task.root_id === currentRoot
  );
}
export function validTaskView(value: unknown): value is DownloadTaskView {
  if (!value || typeof value !== "object") return false;
  const view = value as DownloadTaskView;
  if (
    typeof view.revision !== "string" ||
    !/^\d{1,20}$/.test(view.revision) ||
    !Array.isArray(view.tasks) ||
    view.tasks.length > 256 ||
    !Array.isArray(view.blockedRootIds) ||
    !view.blockedRootIds.every((id) => typeof id === "string") ||
    !Number.isInteger(view.runningLimit) ||
    view.runningLimit < 1 ||
    view.runningLimit > 64 ||
    !Number.isInteger(view.pendingLimit) ||
    view.pendingLimit < 1 ||
    view.pendingLimit > 1024
  )
    return false;
  const ids = new Set<string>();
  return view.tasks.every((task) => {
    if (
      !task ||
      typeof task !== "object" ||
      typeof task.task_id !== "string" ||
      !task.task_id ||
      ids.has(task.task_id) ||
      ![
        "queued",
        "preparing",
        "downloading",
        "processing",
        "complete",
        "error",
        "cancelled",
      ].includes(task.stage) ||
      typeof task.message !== "string" ||
      !(task.version === null || typeof task.version === "string")
    )
      return false;
    ids.add(task.task_id);
    return (
      [task.completed, task.total, task.bytes_done, task.bytes_total].every(
        (n) => Number.isFinite(n) && n >= 0,
      ) &&
      (task.network_bytes === undefined ||
        (Number.isFinite(task.network_bytes) && task.network_bytes >= 0))
    );
  });
}
export function taskRevisionOlder(a: string, b: string): boolean {
  return BigInt(a) < BigInt(b);
}
/** A snapshot is authoritative for membership, while a retained terminal ID
 * cannot be revived by a delayed status/cancel projection for the same job. */
export function adoptTaskView(
  previous: DownloadTaskView,
  next: DownloadTaskView,
): DownloadTaskView {
  if (taskRevisionOlder(next.revision, previous.revision)) return previous;
  const known = new Map(
    previous.tasks.map((task) => [taskIdentity(task), task]),
  );
  return {
    ...next,
    tasks: next.tasks.map((task) => {
      const old = known.get(taskIdentity(task));
      return old && taskIsTerminal(old) && old.stage !== task.stage
        ? old
        : task;
    }),
  };
}
export function taskAggregate(
  tasks: readonly DownloadStatus[],
): DownloadStatus {
  const active = tasks.filter(taskIsActive);
  const executing = active.filter((task) => task.stage !== "queued");
  const measurable = executing.filter(
    (task) => Number.isFinite(task.progress) || task.total > 0,
  );
  return {
    stage: executing.length ? "downloading" : active.length ? "queued" : "idle",
    message: "",
    version: null,
    task_id: "aggregate",
    completed: active.reduce((sum, task) => sum + task.completed, 0),
    total: active.reduce((sum, task) => sum + task.total, 0),
    bytes_done: active.reduce((sum, task) => sum + task.bytes_done, 0),
    bytes_total: active.reduce((sum, task) => sum + task.bytes_total, 0),
    progress:
      active.length && measurable.length
        ? measurable.reduce(
            (sum, task) =>
              sum +
              Math.max(
                0,
                Math.min(1, task.progress ?? task.completed / task.total),
              ),
            0,
          ) / active.length
        : undefined,
  };
}

/** The page owns an origin for an active set, not a single last-written slot.
 * New work joins while the page remains open. Every owned queued/running job
 * must terminate before returning; errors retain review and navigation releases
 * the lease. Retained backend history cannot reopen a completed page. */
export class TaskPageOwner {
  private pending: Set<string> | null = null;
  private outcomes = new Map<string, DownloadStatus>();
  completion: readonly DownloadStatus[] = [];
  historyRetired = false;
  open(value: DownloadStatus | readonly DownloadStatus[]): boolean {
    const tasks = Array.isArray(value) ? value : [value as DownloadStatus];
    const active = tasks.filter(taskIsActive);
    if (!tasks.some(taskHasFloatingEntry)) return false;
    if (!this.pending) {
      this.pending = active.length ? new Set(active.map(taskIdentity)) : null;
      this.outcomes.clear();
      this.completion = [];
      this.historyRetired = false;
    } else active.forEach((task) => this.pending!.add(taskIdentity(task)));
    return true;
  }
  leave(): void {
    this.pending = null;
    this.outcomes.clear();
    this.completion = [];
    this.historyRetired = false;
  }
  takeCompletion(value: DownloadStatus | readonly DownloadStatus[]): boolean {
    if (!this.pending) return false;
    const tasks = Array.isArray(value) ? value : [value as DownloadStatus];
    for (const task of tasks)
      if (taskIsActive(task)) this.pending.add(taskIdentity(task));
    if (Array.isArray(value)) {
      // Active entries are never evicted by native. An absent leased ID has
      // aged out of bounded terminal history; retire ownership without making
      // up a success or cancellation result.
      const retained = new Set(tasks.map(taskIdentity));
      for (const id of this.pending)
        if (!retained.has(id)) {
          this.pending.delete(id);
          this.historyRetired = true;
        }
    }
    for (const task of tasks) {
      const id = taskIdentity(task);
      if (this.pending.has(id) && taskIsTerminal(task)) {
        this.pending.delete(id);
        this.outcomes.set(id, task);
      }
    }
    if (
      this.pending.size ||
      tasks.some((task) => task.stage === "error") ||
      (!this.outcomes.size && !this.historyRetired)
    )
      return false;
    this.completion = [...this.outcomes.values()];
    this.pending = null;
    this.outcomes.clear();
    return true;
  }
}
