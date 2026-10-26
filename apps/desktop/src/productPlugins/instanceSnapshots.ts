/** An instance-bound port over the existing checked native export transaction.
 * Plugins choose a preset, not a filesystem path or raw native command. A
 * preview owns one private submission; changing targets retires that authority.
 * Already admitted exports remain owned by the normal launcher task service.
 */
import type { Api } from "../types";

export type SnapshotMode = "backup" | "share";
export type SnapshotPreview = Readonly<{
  bytes: number;
  fileCount: number;
  warnings: readonly string[];
}>;
export type InstanceSnapshotConnection = {
  prepare(mode: SnapshotMode): Promise<SnapshotPreview>;
  start(
    preview: SnapshotPreview,
    isActive: () => boolean,
  ): Promise<string | null>;
};

export function snapshotVersion(mode: SnapshotMode, time = new Date()) {
  if (mode !== "backup" && mode !== "share")
    throw Error("Invalid snapshot preset");
  return `${mode}-${time.toISOString().replace(/[-:.]/g, "")}`;
}

type NativePlan = {
  revision: string;
  bytes: number;
  file_count: number;
  instance_id: string;
  warnings: string[];
};
export function createInstanceSnapshotConnection({
  api,
  id,
  isCurrent,
  onTask,
}: {
  api: Api;
  id: string;
  isCurrent(): boolean;
  onTask(id: string, reveal: boolean): void;
}): InstanceSnapshotConnection {
  const submissions = new WeakMap<
    SnapshotPreview,
    { id: string; mode: SnapshotMode; version: string; revision: string }
  >();
  const requireCurrent = () => {
    if (!isCurrent()) throw Error("Instance export context retired");
  };
  return {
    async prepare(mode) {
      requireCurrent();
      const version = snapshotVersion(mode);
      const plan = await api<NativePlan>("instance_snapshot_prepare", {
        id,
        mode,
        version,
      });
      requireCurrent();
      if (plan.instance_id !== id)
        throw Error("Instance export target changed");
      // The component sees only counts and warnings. Retaining our own copy
      // prevents a displayed preview from being edited into a new submission.
      const submission = { id, mode, version, revision: plan.revision };
      const preview = Object.freeze({
        bytes: plan.bytes,
        fileCount: plan.file_count,
        warnings: Object.freeze([...plan.warnings]),
      });
      submissions.set(preview, submission);
      return preview;
    },
    async start(preview, isActive) {
      requireCurrent();
      if (!isActive()) throw Error("Instance export plugin retired");
      const plan = submissions.get(preview);
      if (!plan) throw Error("Inspect the export again before saving");
      submissions.delete(preview);
      const task = await api<string | null>("instance_snapshot_start", plan);
      // A system save dialog can outlive navigation or plugin retirement. Track
      // accepted work, but never pull a different page into the task manager.
      if (task) onTask(task, isCurrent() && isActive());
      return task;
    },
  };
}
