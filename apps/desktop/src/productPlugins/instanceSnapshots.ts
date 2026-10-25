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

export function snapshotRequest(
  id: string,
  mode: SnapshotMode,
  time = new Date(),
) {
  if (mode !== "backup" && mode !== "share")
    throw Error("Invalid snapshot preset");
  const backup = mode === "backup";
  return {
    name: id,
    version: `${mode}-${time.toISOString().replace(/[-:.]/g, "")}`,
    checks: {
      game: true,
      gameSettings: backup,
      gamePersonal: backup,
      mods: true,
      packData: true,
      modSettings: true,
      maps: backup,
      jeiPersonal: backup,
      guidePersonal: backup,
      resourcepacks: true,
      shaderpacks: true,
      screenshots: false,
      saves: backup,
      server: backup,
      other: false,
      launcher: false,
      bundleAssets: false,
      modrinth: false,
    },
    excluded: {},
  };
}

type NativePlan = {
  revision: string;
  bytes: number;
  file_count: number;
  instance_id: string;
  request: ReturnType<typeof snapshotRequest>;
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
  const submissions = new WeakMap<SnapshotPreview, NativePlan>();
  const requireCurrent = () => {
    if (!isCurrent()) throw Error("Instance export context retired");
  };
  return {
    async prepare(mode) {
      requireCurrent();
      const plan = await api<NativePlan>("instance_export_plan", {
        id,
        request: snapshotRequest(id, mode),
      });
      requireCurrent();
      if (plan.instance_id !== id)
        throw Error("Instance export target changed");
      // The component sees only counts and warnings. Retaining our own copy
      // prevents a displayed preview from being edited into a new submission.
      const submission = structuredClone(plan);
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
      const task = await api<string | null>("instance_export_start", { plan });
      // A system save dialog can outlive navigation or plugin retirement. Track
      // accepted work, but never pull a different page into the task manager.
      if (task) onTask(task, isCurrent() && isActive());
      return task;
    },
  };
}
