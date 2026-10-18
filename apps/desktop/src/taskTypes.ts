/** Native task identity and bounded scheduler snapshot. Game targets and display names
 * are separate; every cancellation and publication belongs to a task ID. */
export type DownloadStep = {
  id: string;
  label: string;
  state: "pending" | "running" | "complete";
  progress?: number | null;
};
export type FileRepairResult = {
  id: string;
  files_downloaded: number;
  files_present: number;
  unsupported_total: number;
  unsupported: { path: string; reason: string }[];
};
export type FileIntegrityResult = {
  id: string;
  files_verified: number;
  files_missing: number;
  files_corrupt: number;
  files_unverifiable: number;
  assets_enumerated: boolean;
  problems_total: number;
  problems: {
    path: string;
    kind: "missing" | "size_mismatch" | "sha1_mismatch" | "unverifiable";
    reason: string;
  }[];
};
export type DownloadStatus = {
  kind?:
    | "install"
    | "modpack_install"
    | "modpack_prepare"
    | "java_install"
    | "instance_reset"
    | "instance_repair"
    | "instance_verify"
    | "instance_export"
    | "instance_rename"
    | "instance_import"
    | "instance_delete"
    | "instance_restore"
    | "resource_operation"
    | "resource_download"
    | "resource_update"
    | "resource_update_restore"
    | "resource_save"
    | "launcher_logs"
    | "toolbox_download"
    | null;
  stage:
    | "queued"
    | "idle"
    | "preparing"
    | "downloading"
    | "processing"
    | "complete"
    | "error"
    | "cancelled";
  phase?: string;
  message: string;
  version: string | null;
  display_name?: string | null;
  completed: number;
  total: number;
  bytes_done: number;
  bytes_total: number;
  network_bytes?: number;
  task_id?: string | null;
  root_id?: string | null;
  root_path?: string | null;
  progress?: number;
  error?: string | null;
  can_cancel?: boolean;
  steps?: DownloadStep[];
  result?:
    | {
        id: string;
        java_major: number;
        files_downloaded: number;
        files_reused: number;
      }
    | FileRepairResult
    | FileIntegrityResult
    | null;
};
export const idleDownload: DownloadStatus = {
  stage: "idle",
  message: "",
  version: null,
  completed: 0,
  total: 0,
  bytes_done: 0,
  bytes_total: 0,
};

export type DownloadTaskView = {
  revision: string;
  tasks: DownloadStatus[];
  runningLimit: number;
  pendingLimit: number;
  blockedRootIds: string[];
};
export const emptyTaskView: DownloadTaskView = {
  revision: "0",
  tasks: [],
  runningLimit: 4,
  pendingLimit: 32,
  blockedRootIds: [],
};
