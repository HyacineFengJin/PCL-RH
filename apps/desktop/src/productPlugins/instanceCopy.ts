/** A narrow, selection-bound copy port. Prepared submissions remain private and
 * single-use; changing selection or retiring a plugin revokes old callbacks. */
import type { Api } from "../types";
export type CopyOptions = {
  name: string;
  saves: boolean;
  screenshots: boolean;
};
export type CopyPreview = Readonly<{
  name: string;
  bytes: number;
  fileCount: number;
}>;
export type InstanceCopyConnection = {
  prepare(options: CopyOptions): Promise<CopyPreview>;
  start(preview: CopyPreview, isActive: () => boolean): Promise<string>;
};
type NativePreview = {
  revision: string;
  instance_id: string;
  name: string;
  bytes: number;
  file_count: number;
};
export function createInstanceCopyConnection({
  api,
  id,
  isCurrent,
  onTask,
}: {
  api: Api;
  id: string;
  isCurrent(): boolean;
  onTask(id: string, reveal: boolean): void;
}): InstanceCopyConnection {
  const submissions = new WeakMap<
    CopyPreview,
    { request: CopyOptions; revision: string }
  >();
  const requireCurrent = () => {
    if (!isCurrent()) throw Error("Instance copy context retired");
  };
  return {
    async prepare(options) {
      requireCurrent();
      const request = {
        name: options.name,
        saves: options.saves,
        screenshots: options.screenshots,
      };
      const result = await api<NativePreview>("instance_copy_prepare", {
        id,
        request,
      });
      requireCurrent();
      if (result.instance_id !== id || result.name !== request.name)
        throw Error("Instance copy target changed");
      const preview = Object.freeze({
        name: result.name,
        bytes: result.bytes,
        fileCount: result.file_count,
      });
      submissions.set(preview, { request, revision: result.revision });
      return preview;
    },
    async start(preview, isActive) {
      requireCurrent();
      if (!isActive()) throw Error("Instance copy activation retired");
      const submission = submissions.get(preview);
      if (!submission) throw Error("Instance copy preview expired");
      submissions.delete(preview);
      const task = await api<{ id: string }>("instance_copy_start", {
        id,
        ...submission,
      });
      onTask(task.id, isCurrent() && isActive());
      return task.id;
    },
  };
}
