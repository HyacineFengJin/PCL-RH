/** No paths or native revisions reach the component. Selections and previews
 * are private, connection-bound capabilities, retired with their host target. */
import type { Api } from "../types";
export type BackupSelection = Readonly<{
  filename: string;
  suggestedName: string;
}>;
export type BackupRestorePreview = Readonly<{
  name: string;
  packName: string;
  packVersion: string;
  minecraft: string;
  bytes: number;
  fileCount: number;
  warnings: readonly string[];
}>;
export type BackupRestoreConnection = {
  pick(): Promise<BackupSelection | null>;
  prepare(
    selection: BackupSelection,
    name: string,
  ): Promise<BackupRestorePreview>;
  start(
    preview: BackupRestorePreview,
    isActive: () => boolean,
  ): Promise<string>;
};
export function createBackupRestoreConnection({
  api,
  isCurrent,
  onTask,
}: {
  api: Api;
  isCurrent(): boolean;
  onTask(id: string, reveal: boolean): void;
}): BackupRestoreConnection {
  const sources = new WeakMap<BackupSelection, string>();
  const submissions = new WeakMap<
    BackupRestorePreview,
    { source: string; name: string; revision: string }
  >();
  const requireCurrent = () => {
    if (!isCurrent()) throw Error("Backup restore context retired");
  };
  return {
    async pick() {
      requireCurrent();
      const result = await api<{
        status: string;
        source?: string;
        filename?: string;
        suggested_name?: string;
        message?: string;
      }>("instance_snapshot_pick");
      requireCurrent();
      if (result.status === "cancelled") return null;
      if (
        result.status !== "selected" ||
        !result.source ||
        !result.filename ||
        !result.suggested_name
      )
        throw Error(result.message || "Backup selection unavailable");
      const selection = Object.freeze({
        filename: result.filename,
        suggestedName: result.suggested_name,
      });
      sources.set(selection, result.source);
      return selection;
    },
    async prepare(selection, name) {
      requireCurrent();
      const source = sources.get(selection);
      if (!source) throw Error("Backup selection expired");
      const result = await api<{
        revision: string;
        name: string;
        pack_name: string;
        pack_version: string;
        minecraft: string;
        bytes: number;
        file_count: number;
        warnings: string[];
      }>("instance_snapshot_restore_prepare", { source, name });
      requireCurrent();
      if (result.name !== name || !result.revision)
        throw Error("Backup restore target changed");
      const preview = Object.freeze({
        name,
        packName: result.pack_name,
        packVersion: result.pack_version,
        minecraft: result.minecraft,
        bytes: result.bytes,
        fileCount: result.file_count,
        warnings: Object.freeze([...result.warnings]),
      });
      submissions.set(preview, { source, name, revision: result.revision });
      return preview;
    },
    async start(preview, isActive) {
      requireCurrent();
      if (!isActive()) throw Error("Backup restore plugin retired");
      const request = submissions.get(preview);
      if (!request) throw Error("Inspect the backup again before restoring");
      submissions.delete(preview);
      const task = await api<{ id: string }>(
        "instance_snapshot_restore_start",
        request,
      );
      onTask(task.id, isCurrent() && isActive());
      return task.id;
    },
  };
}
