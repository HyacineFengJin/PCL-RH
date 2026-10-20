import type { Api } from "./types";
import type {
  PorterSource,
  PorterDomainResult,
} from "./experimentalPorterTypes";
export type ExperimentalPage = "extensions" | "maker" | "porter";
export type ToolsPage =
  "toolbox" | ExperimentalPage | "ai" | "marketplace" | "projects";
export type ExperimentalNavigate = (
  target: "launch" | "instances" | "downloads" | "tools" | "settings",
) => void;
export type ExtensionScope = "launcher" | "rh-code" | "both";
export type ExtensionEntry = {
  id: string;
  name: string;
  version: string;
  state: string;
  grants: string[];
  publisher: string;
  digest: string;
  enabled: boolean;
  scope: ExtensionScope;
  slots: string[];
  generation: number;
};
export type ExtensionReview = {
  token: string;
  id: string;
  name: string;
  version: string;
  publisher: string;
  digest: string;
  scope: ExtensionScope;
  slots: string[];
  capabilities: {
    id: string;
    reason: string;
    required: boolean;
    currentlyGranted: boolean;
  }[];
};
/** Read-only foreign manifest/package projection, with no review token. */
export type ExtensionCompatibilityReport = {
  kind: "compatibility-report";
  ecosystem: "pcl-n" | "pcl-nex";
  id: string;
  name: string;
  version: string;
  publisher: string | null;
  digest: string;
  loadable: false;
  codeExecuted: false;
  signatureVerified: false;
  entryAssembly: string;
  entryType: string | null;
  apiRange: string | null;
  sdkProbeVersion: string | null;
  requirements: {
    id: string;
    range: string;
    required: boolean;
    coverage: "probe-only" | "unavailable";
  }[];
  permissions: { id: string; reason: string; required: boolean }[];
  dependencies: { id: string; range: string; required: boolean }[];
  platformDeclarations: string[];
  mixinConfigs: string[];
  experimentalFeatures?: string[];
  /** Package directory claims; no payload or signature verification. */
  archive?: {
    format: "pnp" | "pclx";
    bytes: number;
    entryCount: number;
    declaredUncompressedBytes: number;
    payloadVerified: false;
  };
  findings: (
    | "n-probe-only"
    | "n-ui-unavailable"
    | "n-native-unavailable"
    | "nex-host-unavailable"
    | "ranges-unverified"
  )[];
};
export type ExtensionCard = {
  extensionId: string;
  extensionName: string;
  id: string;
  title: string;
  text: string;
  actions: { id: string; label: string; enabled: boolean }[];
  digest?: string;
  generation?: number;
  scope?: ExtensionScope;
  fields?: ExtensionField[];
};
export type ExtensionCardFilter = { extensionId: string; cardId: string };
/** Read-only RH Code metadata. Executable views and native intent arguments
 * belong to the trusted host and never come from a community manifest. */
export type ExtensionPanel = ExtensionCardFilter & {
  viewId: string;
  title: string;
  extensionName: string;
  digest: string;
  generation: number;
};
export type ExtensionCatalogAction = ExtensionCardFilter & {
  extensionName: string;
  cardTitle: string;
  id: string;
  label: string;
  kind: string;
  enabled: boolean;
  unavailableReason: string | null;
  digest: string;
  generation: number;
};
export type ExtensionCatalog = {
  panels: ExtensionPanel[];
  actions: ExtensionCatalogAction[];
};
export type ExtensionInstanceSummary = {
  minecraftVersion: string;
  loader: string;
  modCount: number;
  isolated: boolean;
};
export type ExtensionSetting =
  | {
      id: string;
      label: string;
      type: "text";
      default: string;
      maxLength: number;
      multiline?: boolean;
    }
  | { id: string; label: string; type: "toggle"; default: boolean }
  | {
      id: string;
      label: string;
      type: "select";
      default: string;
      options: { value: string; label: string }[];
    }
  | {
      id: string;
      label: string;
      type: "number";
      default: number;
      min: number;
      max: number;
    };
export type ExtensionField = ExtensionSetting & {
  value: string | boolean | number;
};
export type JobSummary = {
  jobId: string;
  revision: number;
  workflow: "maker" | "porter" | "harness";
  status: string;
  mode: "template" | "live";
  updatedAt: string;
  artifactState: string;
  blockedReason: string | null;
};
export type Artifact = {
  operationId: string;
  directory: string;
  files: string[];
};
/** Native build receipts describe one immutable project copy. Model text and
 * the current editor buffer never establish which source was compiled. */
export type BuildRunView = {
  runId: string;
  jobId: string;
  recipe: "javac" | "gradle";
  target: "classes" | "build" | "test";
  status: string;
  revision: number;
  sourceFingerprint: string;
  proposalId?: string | null;
  proposalDigest?: string | null;
  startedAt: string;
  finishedAt?: string | null;
  exitCode?: number | null;
  signal?: string | null;
  logBytes: number;
  logTruncated?: boolean;
  artifactCount?: number;
  artifactTruncated?: boolean;
  artifacts: { path: string; size: number; sha256: string }[];
  actualGeneratedCodeExecuted?: boolean;
  error?: { code: string; message: string } | null;
};
export type BuildLogView = {
  content: string;
  nextOffset: number | null;
  truncated: boolean;
  totalBytes: number;
};
export type JobView = {
  summary: JobSummary;
  mode: string;
  result: unknown;
  output?: string;
  activity?: import("./MakerAgentState").MakerActivity[];
  error?: { message: string };
  artifacts: Artifact[];
  reviews: string[];
  buildRuns?: BuildRunView[];
  porterSource?: PorterSource;
};
export type ReviewView = {
  reviewId: string;
  reviewDigest: string;
  status: string;
  kind: string;
  preview: {
    diffs?: { path: string; diff?: string }[];
    changes?: { path: string; unified_diff: string }[];
    warnings?: string[];
    locked_files?: string[];
    domain_recipe?: PorterDomainResult;
  };
};
export type PiCatalogModel = {
  id: string;
  name: string;
  api: string;
  baseUrl: string;
  reasoning: boolean;
  supportsImages?: boolean;
  contextWindow: number;
  maxTokens: number;
  cost: { input: number; output: number } | null;
};
export type PiCatalog = {
  providers: { id: string; name: string; models: PiCatalogModel[] }[];
};
export type PiSelection = {
  provider: string;
  model: string;
  api: string;
  baseUrl: string;
  contextWindow: number;
  thinking: string;
  keyConfigured: boolean;
  supportsImages?: boolean;
  cost: { input: number; output: number } | null;
  costKnown: boolean;
};
export type PiWorkLimits = {
  modelTurns: number;
  toolCalls: number;
  maximumOutputTokens: number;
  wallTimeMs: number;
  estimatedBudgetUsd: number;
};
export type AiPreset = {
  id: string;
  name: string;
  selection: PiSelection;
  limits: PiWorkLimits;
};
export type AiPresetStore = {
  selectedId: string | null;
  presets: AiPreset[];
  defaults: PiWorkLimits;
  ranges: Record<keyof PiWorkLimits, [number, number]>;
  warning: string | null;
};
export type EngineStatus = {
  versions: Record<string, string>;
  jobs: JobSummary[];
  liveConfigured: boolean;
  liveAvailable: boolean;
  aiPresets: AiPresetStore;
  liveSelection: PiSelection | null;
  workspace: string;
  storeWarning: string | null;
};
export type MakerSpec = {
  schema_version: number;
  target: string;
  mod_id: string;
  name: string;
  description: string;
  items: {
    id: string;
    names: { en_us: string; zh_cn: string };
    color: string;
    max_count: number;
    recipe: { ingredients: string[]; count: number } | null;
  }[];
};
export type ExperimentalOperation =
  | "rh_code_run"
  | "rh_code_start"
  | "harness_build_log"
  | "maker_thread_read"
  | "maker_threads"
  | "maker_thread_run"
  | "maker_thread_apply"
  | "maker_thread_archive"
  | "status"
  | "projects_list"
  | "project_create"
  | "project_update"
  | "project_units"
  | "project_unit_read"
  | "project_unit_save"
  | "project_protection"
  | "project_files"
  | "project_file_read"
  | "project_search"
  | "project_compare"
  | "project_compare_file"
  | "project_open"
  | "project_continue"
  | "catalog"
  | "extensions_list"
  | "extensions_review"
  | "extensions_confirm"
  | "extensions_cancel"
  | "extensions_revoke"
  | "extensions_disable"
  | "extensions_enable"
  | "extensions_remove"
  | "extensions_rereview"
  | "extensions_safe_mode"
  | "extensions_cards"
  | "extensions_catalog"
  | "extensions_action"
  | "extensions_settings"
  | "source_import"
  | "provider_catalog"
  | "provider_presets"
  | "provider_preset_save"
  | "provider_preset_select"
  | "provider_preset_delete"
  | "provider_start"
  | "provider_stop"
  | "job_create"
  | "job_read"
  | "job_cancel"
  | "artifact_read"
  | "maker_review"
  | "porter_recipe"
  | "porter_review"
  | "porter_origin_resolve"
  | "porter_projects"
  | "porter_project_create"
  | "porter_project_read"
  | "porter_project_message"
  | "porter_project_message_resolve"
  | "porter_project_source"
  | "porter_project_archive"
  | "porter_project_configure"
  | "porter_project_round"
  | "porter_project_versions"
  | "porter_project_compare"
  | "review_read"
  | "review_apply"
  | "review_cancel"
  | "report_read";
export const experimentalCall = <T>(
  api: Api,
  operation: ExperimentalOperation,
  args: Record<string, unknown> = {},
) => api<T>("experimental_call", { operation, args });
