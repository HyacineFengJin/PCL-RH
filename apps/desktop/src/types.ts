export type InstanceMetadata = {
  description: string;
  favorite: boolean;
  icon: "auto" | "grass" | "forge" | "neoforge" | "command" | "steve";
  category: "auto" | "vanilla" | "forge" | "neoforge" | "fabric" | "quilt";
};
export type MetaView = InstanceMetadata & { revision: string };
export type Instance = {
  id: string;
  minecraft_version: string;
  loader: string;
  java_major: number;
  mod_count: number;
  isolated: boolean;
  metadata?: InstanceMetadata;
  metadata_revision?: string;
};
export type IsolationPolicy = "automatic" | "shared" | "isolated";
export type Settings = {
  revision?: string;
  root_id?: string | null;
  root: string;
  player: string;
  memory_gib: number;
  isolation?: IsolationPolicy;
  selected: string | null;
  overrides: Record<string, number>;
  java?: JavaSelection;
  java_paths?: string[];
  java_overrides?: Record<string, JavaSelection>;
  isolation_overrides?: Record<string, IsolationPolicy>;
  window_title_overrides?: Record<string, string>;
};
export type JavaSelection = { mode: "auto" } | { mode: "manual"; path: string };
export type JavaRuntime = {
  path: string;
  major: number;
  vendor: string;
  arch: string;
};
export type JavaCatalog = {
  runtimes: JavaRuntime[];
  unavailable: { path: string; error: string }[];
};
export type JavaAddResult = {
  status: "selected" | "cancelled" | "unavailable";
  settings?: Settings;
  message?: string;
};
export type RootSummary = {
  id: string;
  name: string;
  path: string;
  selected: string | null;
  overrides?: Record<string, number>;
  java_overrides?: Record<string, JavaSelection>;
  isolation_overrides?: Record<string, IsolationPolicy>;
  window_title_overrides?: Record<string, string>;
  available: boolean;
  error?: string | null;
};
export type Api = <T>(
  command: string,
  args?: Record<string, unknown>,
) => Promise<T>;
export type InstanceImportChoice = {
  status: "selected" | "cancelled" | "unavailable";
  source?: string | null;
  suggested_name?: string | null;
  message?: string | null;
};
export type InstanceImportBasePlan = {
  revision: string;
  name: string;
  pack_name: string;
  pack_version: string;
  minecraft: string;
  file_count: number;
  bytes: number;
  reused_files: number;
  warnings: string[];
};
/** Existing exported ZIP imports omit format and keep their prepare/start DTO. */
export type InstanceZipImportPlan = InstanceImportBasePlan & {
  format?: undefined;
};
/** Pack facts contain no writable paths or URLs. An installable plan carries a
 * native one-use confirmation token; edits require a new native preparation. */
export type InstancePackPreview = {
  summary?: string;
  dependencies: { id: string; version: string; supported: boolean }[];
  files: {
    path: string;
    size: number;
    client: "required" | "optional" | "unsupported";
    selected: boolean;
    overridden: boolean;
  }[];
  required_files: number;
  optional_files: number;
  excluded_files: number;
  download_bytes: number;
  override_files: number;
  override_bytes: number;
  client_overrides: number;
  shadowed_files: number;
  blockers: string[];
};
export type InstancePackImportPlan = InstanceImportBasePlan & {
  format:
    "modrinth" | "curseforge" | "mcbbs" | "hmcl" | "multimc" | "ready_game";
  installable: boolean;
  preview: InstancePackPreview;
};
export type InstanceImportPlan = InstanceZipImportPlan | InstancePackImportPlan;
export type InstanceDeletePlan = {
  id: string;
  root_id: string;
  revision: string;
  total_files: number;
  total_bytes: number;
};
export type InstanceDeletedEntry = {
  operation_id: string;
  id: string;
  root_id: string;
  original_root_id?: string;
  created_ms: number;
  state: string;
  can_restore: boolean;
  revision: string | null;
  warning: string | null;
};
