import { t } from "./i18n";
import type { Instance } from "./types";

export type ResourceInstallTarget = Pick<
  Instance,
  "id" | "minecraft_version" | "loader"
>;

/** Only provider identifiers cross the command boundary. URLs, hashes, resource
 * kinds and compatibility facts are resolved again by the backend. */
export type ResourceInstallRequest = {
  source?: "Modrinth" | "CurseForge";
  project_id: string;
  version_id: string;
  file_name: string | null;
};
export type ResourceInstallFile = {
  project_id: string;
  version_id: string;
  title: string;
  kind: "mods" | "resourcepacks" | "shaderpacks";
  file_name: string;
  size: number;
  sha512: string;
  required: boolean;
  reused: boolean;
  existing_file_name: string | null;
};
export type ResourceInstallPlan = {
  root_id: string;
  instance_id: string;
  minecraft_version: string;
  loader: string;
  revision: string;
  files: ResourceInstallFile[];
  dependencies: {
    from_project_id: string;
    from_version_id: string;
    project_id: string | null;
    version_id: string | null;
    file_name: string | null;
    dependency_type: string;
  }[];
  warnings: string[];
  total_bytes: number;
  download_bytes: number;
};

export function resourceInstallRequest(
  request: ResourceInstallRequest,
): ResourceInstallRequest {
  if (request.source === "CurseForge")
    throw new Error(t("community.curseforgeDownloadOnly"));
  return {
    project_id: request.project_id,
    version_id: request.version_id,
    file_name: request.file_name ?? null,
  };
}

/** A read response must still describe the exact visible target and selection.
 * This protects confirmation ownership; the server rechecks authority at start. */
export function checkResourceInstallPlan(
  plan: ResourceInstallPlan,
  rootId: string,
  target: ResourceInstallTarget,
  request: ResourceInstallRequest,
) {
  const targetLoader = target.loader.split(/\s+/)[0].toLowerCase();
  if (
    plan.root_id !== rootId ||
    plan.instance_id !== target.id ||
    plan.minecraft_version !== target.minecraft_version ||
    plan.loader !== (targetLoader === "vanilla" ? "minecraft" : targetLoader) ||
    !plan.revision ||
    !Array.isArray(plan.files) ||
    !plan.files.length ||
    plan.files.length > 128 ||
    !plan.files.some(
      (file) =>
        !file.required &&
        file.project_id === request.project_id &&
        file.version_id === request.version_id &&
        (!request.file_name || file.file_name === request.file_name),
    )
  )
    throw new Error(t("resourceInstall.planChanged"));
  if (
    !Number.isSafeInteger(plan.total_bytes) ||
    !Number.isSafeInteger(plan.download_bytes) ||
    plan.download_bytes < 0 ||
    plan.total_bytes < plan.download_bytes ||
    plan.files.some(
      (file) =>
        !["mods", "resourcepacks", "shaderpacks"].includes(file.kind) ||
        !Number.isSafeInteger(file.size) ||
        file.size < 0,
    ) ||
    plan.files.reduce((sum, file) => sum + file.size, 0) !== plan.total_bytes ||
    plan.files
      .filter((file) => !file.reused)
      .reduce((sum, file) => sum + file.size, 0) !== plan.download_bytes
  )
    throw new Error(t("resourceInstall.invalidSize"));
}
