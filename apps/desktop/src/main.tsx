import { RHCodeHost } from "./RHCodeHost";
import { createLauncherSurfacePort } from "./productPlugins/surfacePort";
import { useLauncherSurfaceHost } from "./productPlugins/launcherHost";
import { useInstanceSnapshotHost } from "./productPlugins/useInstanceSnapshotHost";
import {
  LauncherSurfaceSlot,
  useLauncherSurfaceEffects,
} from "./productPlugins/LauncherSurfaceSlot";
import "./style.css";
import "./rh-code/navigation.css";
import rhMark from "../../../assets/branding/rh-mark.svg";
import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Play,
  Download,
  Settings as SettingsIcon,
  Wrench,
  Minus,
  X,
  ChevronRight,
  ChevronDown,
  FolderOpen,
  Check,
  RefreshCw,
  Search,
  Box,
  UserRound,
  Info,
  FileText,
  Cpu,
  ArrowLeft,
  TriangleAlert,
  LoaderCircle,
  SlidersHorizontal,
  Square,
  ExternalLink,
  Rocket,
  Coffee,
  BookMarked,
  Palette,
  Globe,
  Earth,
  MonitorCog,
  MessageCircle,
  ScrollText,
  Layers,
  Puzzle,
  Sparkles,
  Blocks,
  Heart,
  Gauge,
  Trophy,
  Cat,
  FlaskConical,
  Gift,
  FolderInput,
  PackagePlus,
  Image,
  Server,
  ArrowRight,
  WandSparkles,
  ArrowRightLeft,
  ShieldCheck,
  Network,
  Unplug,
  Shirt,
  Pencil,
  Users,
  UserPlus,
  Pickaxe,
  MoreHorizontal,
  ArrowUp,
  ArrowDown,
} from "lucide-react";
import {
  DownloadPanel,
  idleDownload,
  type DownloadStatus,
} from "./DownloadPanel";
import defaultSkin from "./assets/game-icons/player-skin.png";
import {
  AccountDialog,
  AccountAppearanceDialog,
  AccountAvatar,
} from "./AccountDialog";
import type { AuthState } from "./accountTypes";
import { useAccountPresentation } from "./useAccountPresentation";
import { ExperimentalCards } from "./ExperimentalExtensions";
import {
  launcherSettingsPluginSlot,
  pluginSettingsDestination,
} from "./launcherExtensionSlots";
import type { ExperimentalNavigate, ToolsPage } from "./experimentalTypes";
import type { PorterIntake } from "./experimentalPorterTypes";
import { Toolbox } from "./Toolbox";
import { SettingsPanel } from "./SettingsPanel";
import {
  InstancePanel,
  InstanceSelection,
  instancePages,
  InstanceIcon,
} from "./InstancesPanel";
import { ResourceDetails, type ResourceSummary } from "./ResourceDetails";
import {
  TaskPageOwner,
  taskHasFloatingEntry,
  taskIsActive,
  taskNeedsBootstrap,
  taskNeedsResourceScan,
} from "./taskLifecycle";
import { InstanceImport } from "./InstanceImport";
import { InstanceTrash } from "./InstanceTrash";
import { LaunchInspection } from "./LaunchInspection";
import { LaunchFailure, type FailureRunStatus } from "./LaunchFailure";
import { InstanceLogs } from "./InstanceLogs";
import { ContextMenu } from "./ContextMenu";
import { useDownloadTasks } from "./useDownloadTasks";
import {
  TaskManager,
  TaskStatistics,
  useAggregateDownloadSpeed,
  taskAggregate,
  instanceTaskAction,
} from "./TaskManager";
import "./account-interactions.css";
import { ExtraSettings } from "./ExtraSettings";
import {
  useLauncherPreferences,
  LauncherNavigationContext,
} from "./useLauncherPreferences";
import type { LauncherMenuId } from "./launcherTypes";
import type { ResourceBrowseRequest } from "./resourceBrowse";
import { useLauncherLocal } from "./useLauncherLocal";
import { useLauncherMinecraftNotices } from "./useLauncherMinecraftNotices";
import { useLauncherUpdates } from "./useLauncherUpdates";
import { useLauncherDiscovery } from "./useLauncherDiscovery";
import {
  useLauncherFavorites,
  LauncherFavoritesContext,
} from "./useLauncherFavorites";
import { useLauncherAssets, mediaUrl } from "./useLauncherAssets";
import { configureLocale, t, formatNumber } from "./i18n";
import type {
  Api,
  Instance,
  InstanceMetadata,
  JavaAddResult,
  MetaView,
  RootSummary,
  Settings,
} from "./types";
import "./roots.css";
type Status = {
  stage: string;
  message: string;
  version: string | null;
  pid: number | null;
  exit_code: number | null;
  root_id?: string | null;
  root_path?: string | null;
};
const emptyAuth: AuthState = {
  client_id: "",
  selected: null,
  accounts: [],
  stage: "idle",
  message: "",
  challenge: null,
  warning: null,
};
type State = {
  settings: Settings;
  roots?: RootSummary[];
  scan_issues?: { id: string; message: string }[];
  scan_error?: string | null;
  reset_recovery_error?: string | null;
  rename_recovery_error?: string | null;
  rename_recovery_root_id?: string | null;
  import_recovery_error?: string | null;
  damage_recovery_error?: string | null;
  resource_install_recovery_error?: string | null;
  delete_recovery_error?: string | null;
  delete_recovery_root_id?: string | null;
  config_warning?: string | null;
  instances: Instance[];
  status: Status;
  auth: AuthState;
};
const native = isTauri();
const resourceWrites = new Set([
  "resource_set_enabled",
  "resource_remove",
  "resource_restore",
  "resource_import",
  "resource_recover",
  "resource_update_start",
  "resource_update_restore",
  "resource_install_recover",
  "instance_metadata_update",
  "instance_reset_start",
  "instance_repair_start",
  "instance_verify_start",
  "instance_damage_repair_start",
  "instance_damage_recover",
  "instance_reset_recover",
  "instance_rename_start",
  "instance_rename_recover",
  "instance_export_start",
  "instance_export_config_save",
  "instance_import_start",
  "instance_copy_start",
  "instance_snapshot_start",
  "instance_snapshot_restore_start",
  "instance_pack_start",
  "instance_import_recover",
  "instance_delete_start",
  "instance_restore_start",
  "instance_delete_recover",
]);
const rootCommands = new Set([
  "java_catalog",
  "java_add",
  "java_download_start",
  "launch_game",
  "inspect_instance",
  "memory_preview",
  "instance_logs",
  "instance_read_log",
  "instance_export_log",
  "launch_failure_report",
  "launch_failure_export",
  "launch_failure_ai_preview",
  "launch_failure_ai_start",
  "instance_repair_start",
  "instance_verify_start",
  "instance_damage_repair_start",
  "instance_damage_recover",
  "open_folder",
  "instance_resources",
  "local_resource_info_export",
  "instance_servers",
  "launcher_logs",
  "launcher_read_log",
  "launcher_prepare_log_export",
  "launcher_export_logs",
  "launcher_prepare_log_clear",
  "launcher_clear_logs",
  "launcher_log_recovery",
  "launcher_restore_logs",
  "resource_set_enabled",
  "resource_remove",
  "resource_restore",
  "resource_import",
  "resource_removed",
  "resource_recover",
  "resource_install_plan",
  "resource_update_check",
  "resource_update_plan",
  "resource_update_history",
  "resource_install_start",
  "resource_update_start",
  "resource_update_restore",
  "resource_install_recover",
  "instance_metadata_update",
  "instance_metadata_read",
  "instance_reset_plan",
  "instance_reset_start",
  "instance_reset_recover",
  "instance_rename_prepare",
  "instance_rename_start",
  "instance_rename_recover",
  "instance_export_plan",
  "instance_export_start",
  "instance_export_config_read",
  "instance_export_config_save",
  "instance_import_pick",
  "instance_import_prepare",
  "instance_copy_prepare",
  "instance_snapshot_prepare",
  "instance_snapshot_pick",
  "instance_snapshot_restore_prepare",
  "instance_pack_download",
  "instance_pack_prepare",
  "instance_pack_start",
  "instance_import_start",
  "instance_copy_start",
  "instance_snapshot_start",
  "instance_snapshot_restore_start",
  "instance_import_recover",
  "instance_delete_prepare",
  "instance_delete_start",
  "instance_deleted_list",
  "instance_restore_start",
  "instance_delete_recover",
]);
type PreviewRoot = {
  settings: Settings;
  instances: Instance[];
  resources: unknown[];
  servers: unknown;
  logs: unknown;
  log_contents?: Record<string, string>;
  scan_issues: { id: string; message: string }[];
  scan_error?: string | null;
  recovery_error?: string | null;
};
const previewRoots = new Map<string, PreviewRoot>();
type PreviewResource = {
  name: string;
  file_name: string;
  path: string;
  enabled: boolean;
  fingerprint?: string | null;
};
type PreviewRemoval = {
  id: string;
  files: PreviewResource[];
  created_at: number;
};
const previewRemovals = new Map<string, PreviewRemoval[]>();
let previewResourceSequence = 0;
let previewMetadataSequence = 0;
const defaultInstanceMetadata: InstanceMetadata = {
  description: "",
  favorite: false,
  icon: "auto",
  category: "auto",
};
function previewInstances(
  instances: Instance[],
  root: RootSummary,
): Instance[] {
  return instances.map((instance) => ({
    ...instance,
    metadata: { ...defaultInstanceMetadata, ...instance.metadata },
    metadata_revision:
      instance.metadata_revision ||
      `preview-meta:${JSON.stringify([root.id, root.path, instance.id])}:0`,
  }));
}
let preview:
  | (State & {
      catalog?: unknown[];
      system?: unknown;
      java?: unknown[];
      resources?: unknown[];
      modrinth?: unknown;
      loader_catalog?: unknown;
      launcher_logs?: unknown;
      instance_servers?: unknown;
      contributors?: unknown;
      resource_details?: Record<string, unknown>;
      resource_dependencies?: Record<string, unknown>;
      loader_candidates?: Record<string, unknown>;
      download_status?: DownloadStatus;
      root_data?: Record<string, Partial<PreviewRoot>>;
    })
  | undefined;
function normalizedState(value: State): State {
  const roots = value.roots?.length
    ? value.roots
    : [
        {
          id: value.settings.root_id || "legacy-root",
          name:
            value.settings.root.split(/[\\/]/).filter(Boolean).at(-1) ||
            "Minecraft",
          path: value.settings.root,
          selected: value.settings.selected,
          available: true,
        },
      ];
  const rootId =
    roots.find((root) => root.id === value.settings.root_id)?.id ||
    roots.find((root) => root.path === value.settings.root)?.id ||
    roots[0].id;
  return {
    ...value,
    roots,
    settings: {
      ...value.settings,
      root_id: rootId,
      java: value.settings.java || { mode: "auto" },
      java_paths: value.settings.java_paths || [],
      java_overrides: value.settings.java_overrides || {},
    },
    scan_issues: value.scan_issues || [],
    auth: value.auth || emptyAuth,
  };
}
function preparePreviewRoots() {
  if (!preview || previewRoots.size) return;
  preview = { ...preview, ...normalizedState(preview) };
  for (const root of preview.roots!) {
    const selected = root.id === preview.settings.root_id;
    const stored = preview.root_data?.[root.id];
    previewRoots.set(root.id, {
      settings: {
        ...preview.settings,
        selected: selected ? preview.settings.selected : root.selected,
        overrides: selected
          ? { ...preview.settings.overrides }
          : { ...root.overrides },
        java_overrides: selected
          ? { ...preview.settings.java_overrides }
          : { ...root.java_overrides },
        ...stored?.settings,
        root_id: root.id,
        root: root.path,
      },
      instances: previewInstances(
        stored?.instances || (selected ? preview.instances : []),
        root,
      ),
      resources: stored?.resources || (selected ? preview.resources || [] : []),
      servers:
        stored?.servers || (selected ? preview.instance_servers || [] : []),
      logs: stored?.logs || (selected ? preview.launcher_logs || [] : []),
      log_contents: stored?.log_contents,
      scan_issues:
        stored?.scan_issues || (selected ? preview.scan_issues || [] : []),
      scan_error: stored?.scan_error || (selected ? preview.scan_error : null),
      recovery_error: stored?.recovery_error,
    });
    if (selected) preview.instances = previewRoots.get(root.id)!.instances;
  }
}
function selectPreviewRoot(id: string) {
  const root = preview!.roots!.find((item) => item.id === id);
  const stored = previewRoots.get(id);
  if (!root || !stored) throw new Error("游戏目录不存在");
  const old = previewRoots.get(preview!.settings.root_id!);
  if (old) old.settings = { ...preview!.settings };
  preview = {
    ...preview!,
    settings: {
      ...stored.settings,
      player: preview!.settings.player,
      memory_gib: preview!.settings.memory_gib,
    },
    roots: preview!.roots!.map((item) => ({
      ...item,
      selected: previewRoots.has(item.id)
        ? previewRoots.get(item.id)!.settings.selected
        : item.selected,
    })),
    instances: root.available ? stored.instances : [],
    resources: root.available ? stored.resources : [],
    instance_servers: root.available ? stored.servers : [],
    scan_issues: stored.scan_issues,
    scan_error: root.available
      ? stored.scan_error
      : root.error || "游戏目录暂不可用",
  };
  return normalizedState(preview);
}
async function api<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (native) return invoke<T>(command, args);
  if (!preview)
    preview = await fetch("/preview.json").then((r) => {
      if (!r.ok)
        throw new Error("预览数据不存在，请先运行 npm run preview:data");
      return r.json();
    });
  preparePreviewRoots();
  if (
    command === "instance_metadata_update" ||
    command === "instance_metadata_read"
  ) {
    const rootId = String(args?.rootId || preview!.settings.root_id);
    const root = preview!.roots!.find((item) => item.id === rootId);
    const stored = previewRoots.get(rootId);
    if (!root?.available || !stored) throw new Error("游戏目录暂不可用");
    if (
      command === "instance_metadata_update" &&
      (["preparing", "running"].includes(preview!.status.stage) ||
        ["preparing", "downloading", "processing"].includes(
          preview!.download_status?.stage || "",
        ))
    )
      throw new Error("请在游戏和文件操作结束后修改实例信息");
    const instance = stored.instances.find((item) => item.id === args?.id);
    if (!instance) throw new Error("未找到所选实例");
    if (command === "instance_metadata_read")
      return {
        ...defaultInstanceMetadata,
        ...instance.metadata,
        revision: instance.metadata_revision,
      } as T;
    if (instance.metadata_revision !== args?.revision)
      throw new Error("实例信息已变化，请重新读取后再保存");
    const patch = args?.patch as Partial<InstanceMetadata>;
    if (
      !patch ||
      typeof patch !== "object" ||
      Object.keys(patch).some(
        (key) => !["description", "favorite", "icon", "category"].includes(key),
      ) ||
      !Object.values(patch).some((value) => value !== undefined)
    )
      throw new Error("实例信息修改参数无效");
    if (patch.description !== undefined) {
      if (typeof patch.description !== "string")
        throw new Error("实例描述无效");
      const description = patch.description.replace(/\r\n/g, "\n");
      if (
        Array.from(description).length > 4096 ||
        /[\x00-\x09\x0b-\x1f\x7f-\x9f]/.test(description)
      )
        throw new Error("实例描述过长或包含无效字符");
      patch.description = description.trim();
    }
    if (patch.favorite !== undefined && typeof patch.favorite !== "boolean")
      throw new Error("收藏设置无效");
    if (
      patch.icon !== undefined &&
      !["auto", "grass", "forge", "neoforge", "command", "steve"].includes(
        patch.icon,
      )
    )
      throw new Error("实例图标无效");
    if (
      patch.category !== undefined &&
      !["auto", "vanilla", "forge", "neoforge", "fabric", "quilt"].includes(
        patch.category,
      )
    )
      throw new Error("实例分类无效");
    instance.metadata = {
      ...defaultInstanceMetadata,
      ...instance.metadata,
      ...patch,
    };
    instance.metadata_revision = `preview-meta:${JSON.stringify([root.id, root.path, instance.id])}:${++previewMetadataSequence}`;
    return { ...instance.metadata, revision: instance.metadata_revision } as T;
  }
  if (command === "bootstrap") return normalizedState(preview!) as T;
  if (command === "roots_list") return preview!.roots as T;
  if (command === "root_pick")
    return {
      status: "unavailable",
      message: "界面预览中不能选择本机文件夹，请打开桌面应用。",
    } as T;
  if (command === "root_select")
    return selectPreviewRoot(String(args?.id)) as T;
  if (command === "root_update") {
    const id = String(args?.id);
    const roots = [...preview!.roots!];
    const index = roots.findIndex((root) => root.id === id);
    if (index < 0) throw new Error("游戏目录不存在");
    if (args?.name !== undefined) {
      const name = String(args.name).trim();
      if (!name) throw new Error("请输入文件夹名称");
      roots[index] = { ...roots[index], name };
    }
    if (typeof args?.position === "number") {
      const [root] = roots.splice(index, 1);
      roots.splice(Math.max(0, Math.min(roots.length, args.position)), 0, root);
    }
    preview = { ...preview!, roots };
    return normalizedState(preview) as T;
  }
  if (command === "root_remove") {
    const id = String(args?.id);
    const roots = preview!.roots!;
    if (roots.length <= 1) throw new Error("请至少保留一个游戏目录");
    const index = roots.findIndex((root) => root.id === id);
    if (index < 0) throw new Error("游戏目录不存在");
    if (
      (["preparing", "running"].includes(preview!.status.stage) &&
        preview!.status.root_id === id) ||
      (["preparing", "downloading", "processing"].includes(
        preview!.download_status?.stage || "",
      ) &&
        preview!.download_status?.root_id === id)
    )
      throw new Error("此目录正在使用，请等待任务结束");
    preview = { ...preview!, roots: roots.filter((root) => root.id !== id) };
    previewRoots.delete(id);
    const remaining = preview!.roots!;
    const start = Math.min(index, remaining.length - 1);
    // Follow the native registry order, preferring a usable root before
    // retaining an unavailable neighbour when every remaining root is offline.
    const nextRoot =
      remaining.slice(start).find((root) => root.available) ||
      remaining.slice(0, start).find((root) => root.available) ||
      remaining[start];
    return (
      preview!.settings.root_id === id
        ? selectPreviewRoot(nextRoot.id)
        : normalizedState(preview!)
    ) as T;
  }
  if (command === "auth_status") return (preview!.auth || emptyAuth) as T;
  if (command === "process_status") return preview!.status as T;
  if (command === "download_catalog") return (preview!.catalog || []) as T;
  if (
    command === "launcher_logs" ||
    command === "instance_servers" ||
    command === "instance_resources"
  ) {
    const rootId = String(args?.rootId || preview!.settings.root_id);
    const root = preview!.roots!.find((item) => item.id === rootId);
    const stored = previewRoots.get(rootId);
    if (!root?.available || !stored) return [] as T;
    return (
      command === "launcher_logs"
        ? stored.logs
        : command === "instance_servers"
          ? stored.servers
          : stored.resources
    ) as T;
  }
  if (command === "launcher_read_log") {
    const rootId = String(args?.rootId || preview!.settings.root_id);
    const stored = previewRoots.get(rootId)?.log_contents?.[String(args?.name)];
    if (stored !== undefined) return stored as T;
  }
  if (command === "loader_catalog") {
    const catalogs = preview!.loader_catalog;
    return (
      Array.isArray(catalogs)
        ? catalogs
        : (catalogs as Record<string, unknown> | undefined)?.[
            String(args?.loader)
          ] || []
    ) as T;
  }
  if (command === "loader_candidates")
    return (preview!.loader_candidates?.[
      `${args?.loader}:${args?.minecraft}`
    ] || []) as T;
  if (command === "resource_details" || command === "community_details") {
    const item = preview!.resource_details?.[String(args?.projectId)];
    if (!item) throw new Error("此资源的详情预览数据尚未准备");
    return item as T;
  }
  if (command === "resource_dependencies")
    return (preview!.resource_dependencies?.[String(args?.versionId)] || {
      version_id: args?.versionId,
      dependencies: [],
      truncated: false,
    }) as T;
  if (command === "upstream_contributors")
    return (preview!.contributors || []) as T;
  if (command === "project_feedback") return [] as T;
  if (command === "modrinth_search" || command === "community_search") {
    const catalogs = preview!.modrinth as Record<string, unknown> | undefined;
    return (
      catalogs?.hits
        ? catalogs
        : catalogs?.[String(args?.projectType || "mod")] || { hits: [] }
    ) as T;
  }
  if (command === "system_info") return preview!.system as T;
  if (command === "java_catalog")
    return { runtimes: preview!.java || [], unavailable: [] } as T;
  if (command === "download_tasks") {
    const task = preview!.download_status;
    return {
      revision: "1",
      tasks: task?.task_id && task.stage !== "idle" ? [task] : [],
      runningLimit: 4,
      pendingLimit: 32,
      blockedRootIds:
        task?.root_id &&
        task.stage !== "idle" &&
        !["complete", "error", "cancelled"].includes(task.stage)
          ? [task.root_id]
          : [],
    } as T;
  }
  if (command === "download_status")
    return (preview!.download_status || idleDownload) as T;
  if (command === "resource_import")
    return {
      status: "unavailable",
      changed: 0,
      message: "界面预览中不能选择本机文件，请打开桌面应用。",
    } as T;
  if (
    [
      "resource_removed",
      "resource_set_enabled",
      "resource_remove",
      "resource_restore",
      "resource_recover",
    ].includes(command)
  ) {
    const rootId = String(args?.rootId || preview!.settings.root_id);
    const root = preview!.roots!.find((item) => item.id === rootId);
    const stored = previewRoots.get(rootId);
    if (!root?.available || !stored) throw new Error("游戏目录暂不可用");
    const kind = String(args?.kind);
    const key = `${rootId}:${args?.id}:${kind}`;
    const removed = previewRemovals.get(key) || [];
    if (command === "resource_removed") {
      if (stored.recovery_error) throw new Error(stored.recovery_error);
      return removed.map((operation) => ({
        ...operation,
        files: operation.files.map((file) => file.file_name),
      })) as T;
    }
    if (["preparing", "running"].includes(preview!.status.stage))
      throw new Error("请在游戏退出后修改资源文件");
    if (
      ["preparing", "downloading", "processing"].includes(
        preview!.download_status?.stage || "",
      )
    )
      throw new Error("已有文件写入任务，请等待完成或取消");
    if (!["mods", "resourcepacks", "shaderpacks"].includes(kind))
      throw new Error("此资源类型暂不支持修改");
    if (command === "resource_recover") {
      stored.recovery_error = null;
      return {
        changed: 0,
        undo_id: null,
        message: "已恢复未完成的资源操作，请检查资源列表",
      } as T;
    }
    const entries = stored.resources as PreviewResource[];
    if (command === "resource_restore") {
      const operation = removed.find((item) => item.id === args?.operationId);
      if (!operation) throw new Error("未找到可恢复的删除记录");
      if (
        operation.files.some((file) =>
          entries.some((existing) => existing.file_name === file.file_name),
        )
      )
        throw new Error("恢复位置已有同名文件，请先处理冲突");
      stored.resources = [...entries, ...operation.files];
      previewRemovals.set(
        key,
        removed.filter((item) => item.id !== operation.id),
      );
      return {
        changed: operation.files.length,
        undo_id: null,
        message: "已恢复所选资源",
      } as T;
    }
    const files = args?.files as { file_name: string; fingerprint: string }[];
    if (
      !files?.length ||
      new Set(files.map((file) => file.file_name)).size !== files.length
    )
      throw new Error("所选文件列表无效，请刷新后重试");
    const selected = files.map((file) => {
      const current = entries.find(
        (entry) => entry.file_name === file.file_name,
      );
      if (
        !current ||
        !current.fingerprint ||
        current.fingerprint !== file.fingerprint
      )
        throw new Error("资源文件已变化，请刷新列表后重试");
      return current;
    });
    if (command === "resource_remove") {
      const operation = {
        id: `preview-resource-${++previewResourceSequence}`,
        files: selected.map((file) => ({ ...file })),
        created_at: Date.now(),
      };
      previewRemovals.set(key, [operation, ...removed]);
      stored.resources = entries.filter((file) => !selected.includes(file));
      return {
        changed: selected.length,
        undo_id: operation.id,
        message: "已移除所选资源，可撤销删除",
      } as T;
    }
    if (kind !== "mods") throw new Error("仅模组支持启用或禁用");
    const enabled = args?.enabled === true;
    const plan = selected
      .filter((file) => file.enabled !== enabled)
      .map((file) => ({
        file,
        name: enabled
          ? file.file_name.replace(/\.disabled$/, "")
          : `${file.file_name}.disabled`,
      }));
    if (
      plan.some((item) => entries.some((file) => file.file_name === item.name))
    )
      throw new Error("目标已有同名文件，请先处理冲突");
    stored.resources = entries.map((file) => {
      const item = plan.find((candidate) => candidate.file === file);
      return item
        ? {
            ...file,
            file_name: item.name,
            path: file.path.replace(/[^/]+$/, item.name),
            enabled,
            fingerprint: `preview-${++previewResourceSequence}`,
          }
        : file;
    });
    return {
      changed: plan.length,
      undo_id: null,
      message: enabled ? "已启用所选模组" : "已禁用所选模组",
    } as T;
  }
  if (command === "save_settings") {
    const settings = args!.settings as Settings;
    const rootId = settings.root_id || preview!.settings.root_id!;
    const root = preview!.roots!.find((item) => item.id === rootId);
    if (
      !root ||
      rootId !== preview!.settings.root_id ||
      root.path !== settings.root
    )
      throw new Error("请在实例选择中管理游戏目录");
    previewRoots.get(rootId)!.settings = { ...settings, root_id: rootId };
    preview = {
      ...preview!,
      settings: { ...settings, root_id: rootId },
      roots: preview!.roots!.map((item) =>
        item.id === rootId
          ? {
              ...item,
              selected: settings.selected,
              overrides: settings.overrides,
            }
          : item,
      ),
    };
    return { ...preview!.settings } as T;
  }
  throw new Error("界面预览中不可使用此操作，请打开桌面应用。");
}
function Card({
  title,
  icon,
  children,
  action,
}: {
  title: string;
  icon?: React.ReactNode;
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  const [open, setOpen] = useState(true);
  return (
    <section className={"card " + (!open ? "collapsed" : "")}>
      <div className="card-heading">
        <button
          className="card-toggle"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
        >
          {icon}
          <span>{title}</span>
          {open ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
        </button>
        {action}
      </div>
      {open && <div className="card-content">{children}</div>}
    </section>
  );
}
function Cube({ forge = false }: { forge?: boolean }) {
  return (
    <span className={"cube " + (forge ? "forge" : "")}>
      <Box size={25} strokeWidth={1.5} />
    </span>
  );
}

function App() {
  const [launcherSurfaces] = useState(createLauncherSurfacePort);
  const launcherSurfaceEffects = useLauncherSurfaceEffects(launcherSurfaces);
  const [disabledProductPlugins, setDisabledProductPlugins] = useState<
    string[]
  >([]);
  const [porterIntake, setPorterIntake] = useState<PorterIntake | null>(null);
  const [data, setData] = useState<State | null>(null),
    [error, setError] = useState(""),
    [tab, setTab] = useState("launch"),
    [toolsPage, setToolsPage] = useState<ToolsPage>("projects"),
    [dialog, setDialog] = useState<
      | "versions"
      | "instance"
      | "accounts"
      | "account-type"
      | "account-appearance"
      | null
    >(null),
    [query, setQuery] = useState(""),
    [toast, setToast] = useState(""),
    [draft, setDraft] = useState<Settings | null>(null),
    [override, setOverride] = useState(6);
  const [screen, setScreen] = useState<
    "home" | "versions" | "instance" | "resource" | "tasks"
  >("home");
  const [resource, setResource] = useState<ResourceSummary | null>(null);
  const [resourceOrigin, setResourceOrigin] = useState<"home" | "instance">(
    "home",
  );
  const [taskOrigin, setTaskOrigin] = useState<
    "home" | "versions" | "instance" | "resource"
  >("home");
  const tasks = useDownloadTasks({
    api,
    native,
    onTerminal: handleTaskTerminal,
  });
  const taskView = tasks.view;
  const downloadStatuses = taskView.tasks;
  const [dismissedTasks, setDismissedTasks] = useState<Set<string>>(
    () => new Set(),
  );
  const dismissedSnapshot = React.useRef(dismissedTasks);
  dismissedSnapshot.current = dismissedTasks;
  const visibleTasks = downloadStatuses.filter(
    (task) => taskIsActive(task) || !dismissedTasks.has(task.task_id || ""),
  );
  useEffect(() => {
    dismissedSnapshot.current = new Set();
    setDismissedTasks(new Set());
  }, [tasks.owner]);
  function dismissTask(record: DownloadStatus) {
    const current = tasks.snapshot.current.tasks.find(
      (task) => task.task_id === record.task_id,
    );
    if (
      !current ||
      !(
        current.stage === "error" ||
        (["instance_verify", "instance_damage_repair"].includes(
          current.kind || "",
        ) &&
          current.stage === "complete")
      ) ||
      !current.task_id
    )
      return;
    const next = new Set(dismissedSnapshot.current);
    next.add(current.task_id);
    dismissedSnapshot.current = next;
    setDismissedTasks(next);
  }
  const downloadStatus =
    [...downloadStatuses].reverse().find(taskHasFloatingEntry) ||
    downloadStatuses.at(-1) ||
    idleDownload;
  const taskPageOwner = React.useRef(new TaskPageOwner());
  if (screen !== "tasks") taskPageOwner.current.leave();
  const acceptDownloadStatus = tasks.acceptStatus;
  const downloadBusy = downloadStatuses.some(taskIsActive);
  const queueFull =
    downloadStatuses.filter((task) => task.stage === "queued").length >=
    taskView.pendingLimit;
  const aggregateStatus = taskAggregate(downloadStatuses);
  const speed = useAggregateDownloadSpeed(downloadStatuses);
  const terminalRefresh = React.useRef(false);
  const terminalRead = React.useRef<number | null>(null);
  const [resourceScanGeneration, setResourceScanGeneration] = useState(0);
  function flushTerminalRefresh() {
    if (
      terminalRefresh.current &&
      !rootWorkingRef.current &&
      terminalRead.current === null
    )
      void load();
  }
  function handleTaskTerminal(next: DownloadStatus) {
    if (taskNeedsResourceScan(next, viewContext.current.rootId))
      setResourceScanGeneration((current) => current + 1);
    if (!taskNeedsBootstrap(next, viewContext.current.rootId)) return;
    if (terminalRefresh.current) return;
    terminalRefresh.current = true;
    void Promise.resolve().then(flushTerminalRefresh);
  }
  const [accountType, setAccountType] = useState("");
  const [appearanceId, setAppearanceId] = useState<string | null>(null);
  const [appearanceOrigin, setAppearanceOrigin] = useState<"accounts" | null>(
    null,
  );
  const [profileList, setProfileList] = useState(false);
  const [rootWorking, setRootWorking] = useState(false);
  const rootWorkingRef = React.useRef(false);
  const stateRequest = React.useRef(0);
  const [rootMenu, setRootMenu] = useState<{
    id: string;
    x: number;
    y: number;
  } | null>(null);
  const [rootDialog, setRootDialog] = useState<{
    mode: "rename" | "remove";
    root: RootSummary;
  } | null>(null);
  const [rootName, setRootName] = useState("");
  const [rootError, setRootError] = useState("");
  const [resourceTarget, setResourceTarget] = useState<{
    id: string | null;
  } | null>(null);
  const resourceBusy = resourceTarget !== null;
  const rootId = data?.settings.root_id || null;
  const rootKey = `${rootId || ""}:${data?.settings.root || ""}`;
  const contextKey = React.useRef(rootKey);
  contextKey.current = rootKey;
  const viewContext = React.useRef({
    screen,
    rootKey,
    selected: data?.settings.selected,
    rootId,
    roots: data?.roots,
    deletionRoot: data?.delete_recovery_root_id,
  });
  viewContext.current = {
    screen,
    rootKey,
    selected: data?.settings.selected,
    rootId,
    roots: data?.roots,
    deletionRoot: data?.delete_recovery_root_id,
  };
  const [instanceImport, setInstanceImport] = useState<{
    rootKey: string;
    epoch: number;
  } | null>(null);
  const [inspectionTarget, setInspectionTarget] = useState<{
    rootKey: string;
    id: string;
    epoch: number;
  } | null>(null);
  const [instanceLogsTarget, setInstanceLogsTarget] = useState<{
    rootKey: string;
    id: string;
    epoch: number;
  } | null>(null);
  const failureRun: FailureRunStatus | null =
    data?.status.stage === "error"
      ? {
          ...data.status,
          root_id: data.status.root_id ?? null,
          root_path: data.status.root_path ?? null,
        }
      : null;
  const failureKey = JSON.stringify(failureRun);
  const currentFailureKey = React.useRef(failureKey);
  currentFailureKey.current = failureKey;
  const [failureTarget, setFailureTarget] = useState<{
    rootKey: string;
    epoch: number;
    key: string;
  } | null>(null);
  const [logEntry, setLogEntry] = useState<{
    rootKey: string;
    name: string;
    requestKey: string;
  } | null>(null);
  const logEntrySequence = React.useRef(0);
  const [javaRecommendation, setJavaRecommendation] = useState<{
    rootKey: string;
    id: string;
    major: number;
  } | null>(null);
  const importEntry = React.useRef(instanceImport);
  importEntry.current = instanceImport;
  const archiveRecovery = React.useRef<symbol | null>(null);
  const archiveAdmission = React.useRef({
    blocked: true,
    packBlocked: true,
    packAvailable: false,
    rootAvailable: false,
  });
  function acceptJavaRegistration(
    capturedContext: string,
    revision: unknown,
    result: JavaAddResult,
  ) {
    const saved = result.settings;
    if (result.status !== "selected" || !saved || typeof revision !== "string")
      return false;
    const targetKey = `${saved.root_id || ""}:${saved.root}`;
    if (targetKey !== capturedContext) return false;
    const currentSubmission = (current: Settings) =>
      `${current.root_id || ""}:${current.root}` === targetKey &&
      (current.revision === revision || current.revision === saved.revision);
    // A native chooser can finish after its panel unmounts. The application
    // owns the committed registry/token; a stale view only loses its notices.
    setData((current) =>
      current && currentSubmission(current.settings)
        ? { ...current, settings: saved }
        : current,
    );
    if (contextKey.current === targetKey)
      setDraft((current) =>
        current && currentSubmission(current) ? saved : current,
      );
    return true;
  }
  const rootApi = React.useMemo<Api>(
    () =>
      async <T,>(
        command: string,
        args?: Record<string, unknown>,
      ): Promise<T> => {
        const target = { id: rootId };
        const capturedContext = contextKey.current;
        if (resourceWrites.has(command)) setResourceTarget(target);
        try {
          const result = await api<T>(
            command,
            rootCommands.has(command) ? { ...args, rootId } : args,
          );
          if (
            command === "java_add" &&
            acceptJavaRegistration(
              capturedContext,
              args?.revision,
              result as JavaAddResult,
            )
          )
            // Invalidate older in-flight bootstrap reads, including when the
            // original Java panel has gone away. Root navigation owns its own
            // newer bootstrap while rootWorkingRef is set.
            await load();
          if (
            [
              "instance_import_recover",
              "instance_delete_recover",
              "resource_install_recover",
            ].includes(command) ||
            (["instance_reset_recover", "instance_rename_recover"].includes(
              command,
            ) &&
              contextKey.current === capturedContext)
          )
            await load();
          return result;
        } finally {
          if (resourceWrites.has(command))
            setResourceTarget((current) =>
              current === target ? null : current,
            );
        }
      },
    [rootId],
  );
  function showResource(
    next: ResourceSummary,
    origin: "home" | "instance" = "home",
  ) {
    setResource(next);
    setResourceOrigin(origin);
    setScreen("resource");
  }
  function showTasks() {
    if (
      !taskPageOwner.current.open(
        tasks.snapshot.current.tasks.filter(
          (task) =>
            taskIsActive(task) ||
            !dismissedSnapshot.current.has(task.task_id || ""),
        ),
      )
    )
      return;
    const currentScreen = viewContext.current.screen;
    if (currentScreen !== "tasks") setTaskOrigin(currentScreen);
    setScreen("tasks");
  }
  async function showInstanceTask(id: string, reveal = true) {
    if (!id) return;
    const capturedRoot = contextKey.current,
      capturedNavigation = taskNavigation.current.epoch,
      capturedOwner = tasks.owner;
    tasks.track(id);
    const next = await tasks.refresh();
    if (
      !next ||
      tasks.owner !== capturedOwner ||
      !next.tasks.some(
        (task) => task.task_id === id && taskHasFloatingEntry(task),
      )
    )
      return;
    if (
      reveal &&
      contextKey.current === capturedRoot &&
      taskNavigation.current.epoch === capturedNavigation
    )
      showTasks();
  }
  async function recoverInstanceReset() {
    if (!native || busy || downloadBusy || resourceBusy || !rootAvailable)
      return;
    try {
      await rootApi("instance_reset_recover");
      notify(t("main.recoveredReset"));
    } catch (e) {
      notify(String(e));
    }
  }
  async function recoverResourceInstall() {
    const targetRoot = viewContext.current.rootId;
    if (
      !native ||
      archiveAdmission.current.blocked ||
      rootWorkingRef.current ||
      archiveRecovery.current ||
      !targetRoot ||
      contextKey.current !== rootKey ||
      !viewContext.current.roots?.some(
        (root) => root.id === targetRoot && root.available,
      )
    )
      return;
    const token = Symbol();
    archiveRecovery.current = token;
    const target = { id: targetRoot };
    setResourceTarget(target);
    try {
      await api("resource_install_recover", { rootId: targetRoot });
      await load();
      notify(t("main.recoveredResource"));
    } catch (error) {
      await load();
      notify(String(error));
    } finally {
      if (archiveRecovery.current === token) archiveRecovery.current = null;
      setResourceTarget((current) => (current === target ? null : current));
    }
  }
  async function recoverInstanceRename() {
    const recoveryRoot = data?.rename_recovery_root_id;
    if (!native || busy || downloadBusy || resourceBusy || !recoveryRoot)
      return;
    const target = { id: recoveryRoot };
    setResourceTarget(target);
    try {
      const result = await api<{ message?: string }>(
        "instance_rename_recover",
        {
          rootId: recoveryRoot,
        },
      );
      await load();
      notify(result.message || t("main.recoveredRename"));
    } catch (e) {
      await load();
      notify(String(e));
    } finally {
      setResourceTarget((current) => (current === target ? null : current));
    }
  }
  async function recoverDamage() {
    if (
      !native ||
      archiveAdmission.current.blocked ||
      !rootId ||
      !data ||
      !data.damage_recovery_error
    )
      return;
    const epoch = taskNavigation.current.epoch;
    const key = contextKey.current;
    try {
      const task = await rootApi<{ id: string }>("instance_damage_recover", {
        revision: data.settings.revision,
      });
      showInstanceTask(
        task.id,
        key === contextKey.current && epoch === taskNavigation.current.epoch,
      );
    } catch (error) {
      notify(String(error));
    }
  }
  async function recoverInstanceArchives(kind: "import" | "delete") {
    const recoveryRoot =
      kind === "delete"
        ? viewContext.current.deletionRoot
        : viewContext.current.rootId;
    if (
      !native ||
      archiveAdmission.current.blocked ||
      rootWorkingRef.current ||
      archiveRecovery.current ||
      contextKey.current !== rootKey ||
      !recoveryRoot ||
      !viewContext.current.roots?.some(
        (root) => root.id === recoveryRoot && root.available,
      )
    )
      return;
    const token = Symbol();
    archiveRecovery.current = token;
    const target = { id: recoveryRoot };
    setResourceTarget(target);
    try {
      // A global deletion marker can belong to a different browsed root.
      // Capture its owner for recovery, then bootstrap the current projection.
      await api(`instance_${kind}_recover`, { rootId: recoveryRoot });
      await load();
      notify(
        kind === "delete"
          ? t("main.recoveredDelete")
          : t("main.recoveredImport"),
      );
    } catch (error) {
      await load();
      notify(String(error));
    } finally {
      if (archiveRecovery.current === token) archiveRecovery.current = null;
      setResourceTarget((current) => (current === target ? null : current));
    }
  }
  function openInstanceImport() {
    if (
      !native ||
      archiveAdmission.current.packBlocked ||
      rootWorkingRef.current ||
      !archiveAdmission.current.packAvailable ||
      importEntry.current ||
      viewContext.current.screen !== "versions" ||
      taskNavigation.current.epoch !== navigationEpoch ||
      contextKey.current !== rootKey
    )
      return;
    const choice = { rootKey, epoch: taskNavigation.current.epoch };
    importEntry.current = choice;
    setInstanceImport(choice);
  }
  function finishTaskCancellation(next: DownloadStatus) {
    if (
      !tasks.snapshot.current.tasks.some(
        (task) => task.task_id === next.task_id,
      )
    )
      return;
    notify(
      t("main.cancelled", {
        name: next.display_name || next.version || t("main.game"),
        action: instanceTaskAction(next.kind),
      }),
    );
    // The aggregate page lease handles returning. Cancelling A cannot move the
    // user away while B is still queued/running or needs error review.
  }
  const [instancePage, setInstancePage] = useState("overview");
  const [settingsPage, setSettingsPage] = useState("launch");
  const [downloadPage, setDownloadPage] = useState("minecraft");
  const [resourceBrowse, setResourceBrowse] = useState<
    (ResourceBrowseRequest & { sequence: number }) | undefined
  >();
  const taskNavigation = React.useRef({ key: "", epoch: 0 });
  const resourceNavigation =
    screen === "resource"
      ? `${resource?.source || ""}:${resource?.project_id || resource?.local_path || ""}`
      : "";
  const navigationKey = `${rootKey}:${screen}:${tab}:${toolsPage}:${instancePage}:${data?.settings.selected || ""}:${resourceNavigation}`;
  if (taskNavigation.current.key !== navigationKey) {
    taskNavigation.current = {
      key: navigationKey,
      epoch: taskNavigation.current.epoch + 1,
    };
  }
  const navigationEpoch = taskNavigation.current.epoch;
  useEffect(() => {
    setInstanceLogsTarget((current) =>
      current?.epoch === navigationEpoch ? current : null,
    );
    setInspectionTarget((current) =>
      current?.epoch === navigationEpoch ? current : null,
    );
    setFailureTarget((current) =>
      current?.epoch === navigationEpoch ? current : null,
    );
  }, [navigationEpoch]);
  useEffect(() => {
    setFailureTarget((current) =>
      current?.key === failureKey ? current : null,
    );
  }, [failureKey]);
  useEffect(() => {
    if (screen !== "home" || tab !== "settings" || settingsPage !== "logs")
      setLogEntry(null);
  }, [screen, tab, settingsPage]);
  useEffect(() => {
    const choice = importEntry.current;
    if (
      !choice ||
      (choice.rootKey === rootKey &&
        choice.epoch === navigationEpoch &&
        screen === "versions")
    )
      return;
    importEntry.current = null;
    setInstanceImport((current) => (current === choice ? null : current));
  }, [rootKey, navigationEpoch, screen]);
  const contentRef = React.useRef<HTMLElement>(null);
  useEffect(() => {
    contentRef.current?.scrollTo({ top: 0 });
  }, [screen, tab, settingsPage, downloadPage, instancePage, !!data]);
  const [remember, setRemember] = useState(true);
  const [authWorking, setAuthWorking] = useState(false);
  const [authError, setAuthError] = useState("");
  const accountPresentation = useAccountPresentation(
    api,
    native,
    data?.auth || emptyAuth,
  );
  const notify = (text: string) => {
    setToast(text);
    window.setTimeout(() => setToast(""), 4500);
  };
  useEffect(() => {
    if (
      screen !== "tasks" ||
      tasks.snapshot.current !== taskView ||
      !taskPageOwner.current.takeCompletion(visibleTasks)
    )
      return;
    const completed = taskPageOwner.current.completion;
    const historyRetired = taskPageOwner.current.historyRetired;
    if (historyRetired) void load();
    const onlyCancelledInstall =
      completed.length === 1 &&
      completed[0].kind === "install" &&
      completed[0].stage === "cancelled";
    if (onlyCancelledInstall) {
      setTab("download");
      setDownloadPage("minecraft");
      setTaskOrigin("home");
    }
    setScreen((current) => {
      if (current !== "tasks") return current;
      if (
        tasks.snapshot.current.tasks.some(
          (task) =>
            taskHasFloatingEntry(task) &&
            !dismissedSnapshot.current.has(task.task_id || ""),
        )
      ) {
        taskPageOwner.current.open(
          tasks.snapshot.current.tasks.filter(
            (task) =>
              taskIsActive(task) ||
              !dismissedSnapshot.current.has(task.task_id || ""),
          ),
        );
        return current;
      }
      return onlyCancelledInstall ? "home" : taskOrigin;
    });
    const done = completed.filter((task) => task.stage === "complete");
    if (historyRetired)
      notify(
        t("task.historyRetired") +
          (done.length
            ? " · " + t("task.batchComplete", { count: done.length })
            : ""),
      );
    else if (done.length > 1)
      notify(
        t("task.batchComplete", { count: done.length }) +
          (done.some((task) => task.error)
            ? " · " +
              done
                .filter((task) => task.error)
                .map((task) => task.error)
                .join("; ")
            : ""),
      );
    else if (done[0]?.message) notify(done[0].message);
  }, [taskView, dismissedTasks, screen, taskOrigin]);
  const launcher = useLauncherPreferences(api, native, notify);
  const launcherSurfaceHost = useLauncherSurfaceHost(
    api,
    native && !tasks.closing,
    launcher,
    notify,
  );
  configureLocale(launcher.prefs.localization);
  const launcherMedia = useLauncherAssets(api, native, launcher, notify);
  const launcherLocal = useLauncherLocal(api, native, launcher, notify);
  const launcherFavorites = useLauncherFavorites(api, native, notify);
  const launcherUpdates = useLauncherUpdates(api, native, launcher, notify);
  const minecraftNotices = useLauncherMinecraftNotices({
    api,
    native,
    launcher,
    onNotify: notify,
  });
  const launcherDiscovery = useLauncherDiscovery({
    api,
    native,
    enabled: native,
    launcher,
    onNotify: notify,
    contextKey: `${navigationKey}:${settingsPage}:${downloadPage}`,
    // Clipboard navigation must never replace an open confirmation or a newer
    // user navigation. Native GTK additionally verifies actual keyboard focus.
    canNavigate: () =>
      !!data &&
      !rootWorking &&
      !launcher.busy &&
      !dialog &&
      !rootDialog &&
      !instanceImport &&
      !launcher.confirmationUi &&
      !launcherLocal.ui &&
      !document.querySelector('[role="dialog"]'),
    onResource: (link) =>
      showResource(
        {
          project_id: link.projectIdOrSlug,
          title: link.projectIdOrSlug,
          description: "",
          icon_url: null,
          categories: [],
          display_categories: [],
          versions: [],
          project_type: link.kind,
          source: "Modrinth",
        },
        "home",
      ),
  });
  useEffect(() => {
    if (
      tab !== "launch" &&
      launcher.isHidden(`main.${tab}` as LauncherMenuId)
    ) {
      setTab("launch");
      setScreen("home");
    } else if (
      tab === "settings" &&
      launcher.isHidden(`settings.${settingsPage}` as LauncherMenuId)
    ) {
      setSettingsPage("launch");
      if (launcher.isHidden("settings.launch")) setTab("launch");
    }
    const instanceId =
      instancePage === "litematics" ? "schematics" : instancePage;
    if (
      screen === "instance" &&
      launcher.isHidden(`instance.${instanceId}` as LauncherMenuId)
    )
      setInstancePage("overview");
    if (
      (screen === "instance" || screen === "versions") &&
      launcher.isHidden("feature.instance_management")
    )
      setScreen("home");
  }, [
    launcher.prefs.navigation.hidden_menu_ids,
    launcher.revealed,
    tab,
    screen,
    settingsPage,
    instancePage,
    toolsPage,
  ]);
  function applyState(snapshot: State) {
    const next = normalizedState(snapshot);
    const previous = viewContext.current;
    const missingInstance =
      previous.rootKey ===
        `${next.settings.root_id || ""}:${next.settings.root}` &&
      !!previous.selected &&
      !next.instances.some((instance) => instance.id === previous.selected);
    if (
      !next.instances.some((i) => i.id === next.settings.selected) &&
      next.instances.length
    )
      next.settings.selected =
        next.instances.find((x) => x.mod_count > 0)?.id || next.instances[0].id;
    if (!next.instances.length) next.settings.selected = null;
    // Removing the physical instance invalidates its details even if another
    // instance becomes selected. The task's back destination must agree too.
    if (missingInstance) {
      setScreen((current) => (current === "instance" ? "versions" : current));
      setTaskOrigin((current) =>
        current === "instance" ? "versions" : current,
      );
    }
    setData(next);
    setDraft(next.settings);
    setError("");
  }
  async function load() {
    if (rootWorkingRef.current) return;
    const request = ++stateRequest.current;
    // Keep terminal intent attached to its read until it settles. A root
    // operation can retire that response while its chooser is still open.
    if (terminalRefresh.current) {
      terminalRefresh.current = false;
      terminalRead.current = request;
    }
    try {
      const next = await api<State>("bootstrap");
      if (request === stateRequest.current) applyState(next);
    } catch (e) {
      if (request === stateRequest.current) setError(String(e));
    } finally {
      if (terminalRead.current === request) {
        terminalRead.current = null;
        if (request !== stateRequest.current) terminalRefresh.current = true;
      }
      flushTerminalRefresh();
    }
  }
  async function rootAction(command: string, args: Record<string, unknown>) {
    if (rootWorkingRef.current) return;
    rootWorkingRef.current = true;
    setRootWorking(true);
    setRootError("");
    setRootMenu(null);
    const request = ++stateRequest.current;
    try {
      const next = await api<State>(command, args);
      if (request === stateRequest.current) applyState(next);
      setRootDialog(null);
    } catch (e) {
      if (rootDialog) setRootError(String(e));
      else notify(String(e));
    } finally {
      rootWorkingRef.current = false;
      setRootWorking(false);
      flushTerminalRefresh();
    }
  }
  async function addRoot() {
    if (rootWorkingRef.current) return;
    rootWorkingRef.current = true;
    setRootWorking(true);
    setRootMenu(null);
    const request = ++stateRequest.current;
    try {
      const choice = await api<{
        status: "selected" | "cancelled" | "unavailable";
        path?: string;
        message?: string;
      }>("root_pick");
      if (choice.status === "cancelled") return;
      if (choice.status !== "selected" || !choice.path) {
        notify(choice.message || t("folders.chooserError"));
        return;
      }
      const registered = await api<{ root: RootSummary; bootstrap: State }>(
        "root_register",
        { path: choice.path },
      );
      if (request === stateRequest.current) applyState(registered.bootstrap);
      const next = await api<State>("root_select", { id: registered.root.id });
      if (request === stateRequest.current) applyState(next);
    } catch (e) {
      notify(String(e));
    } finally {
      rootWorkingRef.current = false;
      setRootWorking(false);
      flushTerminalRefresh();
    }
  }
  useEffect(() => {
    load();
    const t = setInterval(
      () =>
        api<Status>("process_status")
          .then((status) => setData((d) => (d ? { ...d, status } : d)))
          .catch(() => {}),
      900,
    );
    const a = setInterval(
      () =>
        api<AuthState>("auth_status")
          .then((auth) => setData((d) => (d ? { ...d, auth } : d)))
          .catch(() => {}),
      1000,
    );
    return () => {
      clearInterval(t);
      clearInterval(a);
    };
  }, []);
  useEffect(() => {
    setInspectionTarget(null);
    setFailureTarget(null);
    setLogEntry(null);
    setJavaRecommendation(null);
    setResource(null);
    setQuery("");
    setRootMenu(null);
    setInstanceImport(null);
    importEntry.current = null;
    setInstancePage("overview");
    setScreen((current) =>
      ["instance", "resource"].includes(current) ? "versions" : current,
    );
    setTaskOrigin((current) =>
      ["instance", "resource"].includes(current) ? "versions" : current,
    );
  }, [rootKey]);
  useEffect(() => {
    if (!rootMenu) return;
    const close = () => setRootMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("resize", close);
    };
  }, [rootMenu]);
  useEffect(() => {
    const fn = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (dialog === "accounts") closeAccountDialog();
        else if (dialog === "account-appearance") setDialog(appearanceOrigin);
        else setDialog(null);
        setRootMenu(null);
        if (!rootWorkingRef.current) setRootDialog(null);
      }
      if (e.key === "Tab") {
        const modal = document.querySelector<HTMLElement>('[role="dialog"]');
        if (!modal) return;
        const items = [
          ...modal.querySelectorAll<HTMLElement>(
            'button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled),summary,[tabindex="0"]',
          ),
        ];
        const visible = items.filter(
          (item) => item.getClientRects().length > 0,
        );
        const first = visible[0],
          last = visible[visible.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last?.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first?.focus();
        }
      }
    };
    window.addEventListener("keydown", fn);
    return () => window.removeEventListener("keydown", fn);
  }, [dialog, native, appearanceOrigin]);
  useEffect(() => {
    if (!dialog && !rootDialog) return;
    const previous = document.activeElement as HTMLElement | null;
    const t = setTimeout(
      () =>
        document
          .querySelector<HTMLElement>(
            '[role="dialog"] input, [role="dialog"] button',
          )
          ?.focus(),
      0,
    );
    return () => {
      clearTimeout(t);
      previous?.focus();
    };
  }, [dialog, rootDialog]);
  async function authAction(command: string, args?: Record<string, unknown>) {
    setAuthWorking(true);
    setAuthError("");
    try {
      await api(command, args);
      const auth = await api<AuthState>("auth_status");
      setData((d) => (d ? { ...d, auth } : d));
    } catch (e) {
      setAuthError(String(e));
    } finally {
      setAuthWorking(false);
    }
  }
  function openAccountDialog() {
    setAccountType(data?.auth.selected ? "premium" : "offline");
    setAuthError("");
    setDialog("accounts");
  }
  function openAccountAppearance(id: string) {
    setAppearanceId(id);
    setAppearanceOrigin(dialog === "accounts" ? "accounts" : null);
    setDialog("account-appearance");
  }
  function closeAccountDialog() {
    // Cancel during code acquisition too, before a late challenge can open a browser.
    if (native) void api("auth_cancel").catch(() => {});
    setDialog(null);
  }
  async function saveOfflineAccount(name: string) {
    if (!data) return;
    setAuthWorking(true);
    setAuthError("");
    try {
      await save({ ...data.settings, player: name });
      await api("auth_select", { id: null });
      const auth = await api<AuthState>("auth_status");
      setData((current) => (current ? { ...current, auth } : current));
      setDialog(null);
    } catch (error) {
      setAuthError(String(error));
    } finally {
      setAuthWorking(false);
    }
  }
  const activeAccount = data?.auth.accounts.find(
    (a) => a.profile.id === data.auth.selected,
  );
  const selected = data?.instances.find((i) => i.id === data.settings.selected),
    busy =
      data?.status.stage === "preparing" || data?.status.stage === "running";
  const processInRoot =
    !!data &&
    (data.status.root_id
      ? data.status.root_id === rootId
      : data.status.root_path
        ? data.status.root_path === data.settings.root
        : true);
  const boundGame =
    !!busy && processInRoot && data?.status.version === selected?.id;
  const installInRoot = !!rootId && taskView.blockedRootIds.includes(rootId);
  const rootOccupied =
    (!!busy && processInRoot) ||
    installInRoot ||
    (resourceBusy && resourceTarget?.id === rootId);
  const selectedRoot = data?.roots?.find((root) => root.id === rootId);
  const rootAvailable = selectedRoot?.available !== false;
  const instanceRecoveryBlocked =
    !!data?.rename_recovery_error ||
    !!data?.reset_recovery_error ||
    !!data?.import_recovery_error ||
    !!data?.damage_recovery_error ||
    !!data?.delete_recovery_error ||
    !!data?.resource_install_recovery_error;
  // Reading a stable log is useful while the game is running. Only preparation,
  // directory/file changes and shutdown prevent this separate read-only entry.
  const instanceLogsDisabled =
    data?.status.stage === "preparing" ||
    downloadBusy ||
    resourceBusy ||
    rootWorking ||
    !rootAvailable ||
    instanceRecoveryBlocked ||
    tasks.closing;
  archiveAdmission.current = {
    blocked: !!busy || downloadBusy || resourceBusy || rootWorking,
    packBlocked: !!busy || resourceBusy || rootWorking,
    packAvailable:
      rootAvailable &&
      !data?.rename_recovery_error &&
      !data?.reset_recovery_error &&
      !data?.damage_recovery_error &&
      !data?.delete_recovery_error &&
      !data?.resource_install_recovery_error &&
      (!data?.import_recovery_error ||
        taskView.tasks.some(
          (task) =>
            task.kind === "modpack_install" &&
            task.root_id === rootId &&
            taskIsActive(task),
        )),
    rootAvailable: rootAvailable && !instanceRecoveryBlocked,
  };
  const instanceSurfaceHost = useInstanceSnapshotHost(
    launcherSurfaceHost,
    rootApi,
    selected?.id,
    `${rootKey}:${data?.settings.revision || ""}:${navigationEpoch}:${instancePage}`,
    native &&
      screen === "instance" &&
      instancePage === "overview" &&
      !busy &&
      !downloadBusy &&
      !resourceBusy &&
      !rootWorking &&
      rootAvailable &&
      !instanceRecoveryBlocked &&
      !tasks.closing,
    showInstanceTask,
  );
  async function save(cfg: Settings) {
    const settings = { ...cfg, root_id: cfg.root_id || rootId };
    const targetKey = `${settings.root_id || ""}:${settings.root}`;
    const saved = await api<Settings>("save_settings", { settings });
    const currentSubmission = (current: Settings) =>
      `${current.root_id || ""}:${current.root}` === targetKey &&
      (current.revision === settings.revision ||
        current.revision === saved.revision);
    setData((d) =>
      d && currentSubmission(d.settings) ? { ...d, settings: saved } : d,
    );
    if (contextKey.current === targetKey) {
      setDraft((current) =>
        current && currentSubmission(current) ? saved : current,
      );
      const isolationChanged =
        settings.isolation !== data?.settings.isolation ||
        JSON.stringify(settings.isolation_overrides || {}) !==
          JSON.stringify(data?.settings.isolation_overrides || {});
      // A policy save changes directory ownership as well as controls. Reload
      // the root's scan so counts and rows describe the same content scope.
      if (isolationChanged) await load();
    }
  }
  async function pick(id: string) {
    if (!data || rootWorking) return;
    const targetKey = rootKey;
    try {
      await save({ ...data.settings, selected: id });
      if (contextKey.current !== targetKey) return;
      setDialog(null);
      setScreen("home");
      setInspectionTarget(null);
    } catch (e) {
      notify(String(e));
    }
  }
  async function launch() {
    if (
      !data ||
      !selected ||
      busy ||
      downloadBusy ||
      resourceBusy ||
      rootWorking ||
      !rootAvailable
    )
      return;
    const targetKey = rootKey;
    try {
      await save(data.settings);
      await rootApi("launch_game", { id: selected.id });
      setData((d) =>
        d && contextKey.current === targetKey
          ? {
              ...d,
              status: {
                ...d.status,
                stage: "preparing",
                message: t("main.checkingLaunch"),
                version: selected.id,
                root_id: rootId,
                root_path: data.settings.root,
              },
            }
          : d,
      );
    } catch (e) {
      notify(String(e));
    }
  }
  async function open(kind: string) {
    try {
      await rootApi("open_folder", { kind, id: selected?.id || null });
    } catch (e) {
      notify(String(e));
    }
  }
  function inspect() {
    if (
      !native ||
      !selected ||
      tasks.closing ||
      busy ||
      downloadBusy ||
      resourceBusy ||
      rootWorking ||
      !rootAvailable ||
      instanceRecoveryBlocked ||
      contextKey.current !== rootKey ||
      viewContext.current.selected !== selected.id
    )
      return;
    setInspectionTarget({ rootKey, id: selected.id, epoch: navigationEpoch });
  }
  function diagnoseFailure() {
    if (
      !native ||
      !failureRun ||
      !processInRoot ||
      !rootAvailable ||
      rootWorking ||
      tasks.closing ||
      contextKey.current !== rootKey ||
      taskNavigation.current.epoch !== navigationEpoch ||
      currentFailureKey.current !== failureKey
    )
      return;
    setFailureTarget({ rootKey, epoch: navigationEpoch, key: failureKey });
  }
  function showInstanceLogs() {
    if (
      !native ||
      !selected ||
      instanceLogsDisabled ||
      contextKey.current !== rootKey ||
      viewContext.current.selected !== selected.id ||
      taskNavigation.current.epoch !== navigationEpoch
    )
      return;
    setInstanceLogsTarget({ rootKey, id: selected.id, epoch: navigationEpoch });
  }
  function instanceSettings() {
    if (!selected || !data) return;
    setInspectionTarget(null);
    setOverride(
      data.settings.overrides[selected.id] || data.settings.memory_gib,
    );
    setInstancePage("overview");
    setScreen("instance");
  }
  function rootOptions(id: string, x: number, y: number) {
    if (rootWorking) return;
    setRootMenu({
      id,
      x: Math.max(8, Math.min(x, window.innerWidth - 210)),
      y: Math.max(56, Math.min(y, window.innerHeight - 184)),
    });
  }
  function editRoot(root: RootSummary, mode: "rename" | "remove") {
    setRootMenu(null);
    setRootName(root.name);
    setRootError("");
    setRootDialog({ root, mode });
  }
  const menuRoot = data?.roots?.find((root) => root.id === rootMenu?.id);
  const menuRootIndex =
    data?.roots?.findIndex((root) => root.id === rootMenu?.id) ?? -1;
  const menuRootOccupied =
    !!menuRoot &&
    ((!!busy &&
      (data?.status.root_id === menuRoot.id ||
        data?.status.root_path === menuRoot.path ||
        (!data?.status.root_id &&
          !data?.status.root_path &&
          menuRoot.id === rootId))) ||
      taskView.blockedRootIds.includes(menuRoot.id) ||
      (resourceBusy && resourceTarget?.id === menuRoot.id));
  const downloadItems = [
    { id: "minecraft", label: "Minecraft", icon: Blocks },
    {
      id: "mods",
      label: t("resources.mods"),
      group: t("main.community"),
      icon: Puzzle,
    },
    { id: "modpacks", label: t("resources.modpacks"), icon: Box },
    { id: "datapacks", label: t("resources.datapacks"), icon: FileText },
    { id: "resourcepacks", label: t("nav.resourcepacks"), icon: Layers },
    { id: "shaders", label: t("nav.shaderpacks"), icon: Sparkles },
    { id: "worlds", label: t("resources.worlds"), icon: Globe },
    { id: "favorites", label: t("resources.favorites"), icon: Heart },
    {
      id: "installer-minecraft",
      label: "Minecraft",
      group: t("main.installers"),
      icon: Box,
    },
    { id: "OptiFine", label: "OptiFine", icon: Gauge },
    { id: "Forge", label: "Forge", icon: Trophy },
    { id: "NeoForge", label: "NeoForge", icon: Cat },
    { id: "Cleanroom", label: "Cleanroom", icon: FlaskConical },
    { id: "Fabric", label: "Fabric", icon: ScrollText },
    {
      id: "Legacy Fabric",
      label: "Legacy Fabric",
      icon: ScrollText,
    },
    { id: "LabyMod", label: "LabyMod", icon: Box },
    { id: "LiteLoader", label: "LiteLoader", icon: Box },
  ];
  const settingsItems = [
    {
      id: "launch",
      label: t("nav.launch"),
      group: t("main.game"),
      icon: Rocket,
    },
    { id: "java", label: t("nav.java"), icon: Coffee },
    { id: "manage", label: t("nav.manage"), icon: BookMarked },
    {
      id: "personalize",
      label: t("nav.personalize"),
      group: t("main.launcher"),
      icon: Palette,
    },
    { id: "language", label: t("nav.language"), icon: Earth },
    { id: "misc", label: t("nav.misc"), icon: MonitorCog },
    { id: "toolbox", label: t("nav.toolbox"), icon: Gift },
    {
      id: "about",
      label: t("nav.about"),
      group: t("nav.about"),
      icon: Info,
    },
    { id: "update", label: t("nav.update"), icon: RefreshCw },
    { id: "feedback", label: t("nav.feedback"), icon: MessageCircle },
    { id: "logs", label: t("nav.logs"), icon: ScrollText },
  ];
  function menu(
    items: {
      id: string;
      label: string;
      group?: string;
      icon: React.ElementType;
      disabled?: boolean;
    }[],
    value: string,
    choose: (s: string) => void,
    prefix?: "settings" | "instance" | "tools",
  ) {
    let group: string | undefined, shownGroup: string | undefined;
    const visible = items.flatMap((item) => {
      if (item.group) group = item.group;
      const id = item.id === "litematics" ? "schematics" : item.id;
      if (prefix && launcher.isHidden(`${prefix}.${id}` as LauncherMenuId))
        return [];
      const next = { ...item, group: group !== shownGroup ? group : undefined };
      shownGroup = group;
      return [next];
    });
    return visible.map((item) => (
      <React.Fragment key={item.id}>
        {item.group && (
          <div className="section-label" title={item.group}>
            {item.group}
          </div>
        )}
        <button
          className={"side-item " + (value === item.id ? "selected" : "")}
          disabled={item.disabled}
          aria-label={item.label}
          title={
            item.disabled
              ? `${item.label} · ${t("main.pageUnavailable")}`
              : item.label
          }
          onClick={() => choose(item.id)}
        >
          <item.icon size={19} />
          <span>{item.label}</span>
        </button>
      </React.Fragment>
    ));
  }
  // Extensions can request fixed page navigation only, never trigger a task.
  const settingsPluginSlot = launcherSettingsPluginSlot(settingsPage);
  const navigateExperimental: ExperimentalNavigate = (target) => {
    const destination = pluginSettingsDestination(target);
    if (destination) {
      setSettingsPage(destination);
      setScreen("home");
      setTab("settings");
      return;
    }
    switch (target) {
      case "launch":
        setScreen("home");
        setTab("launch");
        break;
      case "instances":
        setScreen("versions");
        setTab("launch");
        break;
      case "downloads":
        setScreen("home");
        setTab("download");
        break;
      case "tools":
        setSettingsPage("toolbox");
        setScreen("home");
        setTab("settings");
        break;
      case "settings":
        setScreen("home");
        setTab("settings");
        break;
    }
  };
  return (
    <LauncherFavoritesContext.Provider
      value={{
        ...launcherFavorites,
        open: (entry) =>
          showResource(
            {
              project_id: entry.projectId,
              title: entry.title,
              description: entry.summary,
              icon_url: entry.iconUrl,
              categories: [],
              display_categories: [],
              versions: [],
              project_type: entry.projectType,
              source: "Modrinth",
            },
            "home",
          ),
      }}
    >
      <LauncherNavigationContext.Provider
        value={{
          isHidden: launcher.isHidden,
          modDisplayStyle: launcher.prefs.management.mod_display_style,
          standaloneSaveAvailable: launcher.effects.includes("save_policy"),
          browseResources: (request) => {
            setResourceBrowse((previous) => ({
              ...request,
              sequence: (previous?.sequence || 0) + 1,
            }));
            setDownloadPage(request.section);
            setTab("download");
            setScreen("home");
          },
        }}
      >
        <div
          data-advanced-materials={launcher.prefs.appearance.advanced_materials}
          data-has-background={!!launcherMedia.background}
          data-background-overlay={launcher.prefs.background.color_overlay}
          className={
            "app-shell ce-shell " +
            (screen !== "home"
              ? screen === "versions"
                ? "selection-shell"
                : screen === "resource"
                  ? "resource-shell"
                  : screen === "tasks"
                    ? "tasks-shell"
                    : "instance-shell"
              : `${tab}-shell`)
          }
        >
          {launcherMedia.backgroundUi}
          <ContextMenu
            native={native}
            enabled={launcher.prefs.appearance.custom_context_menu}
            density={launcher.prefs.appearance.context_menu_density}
            scopeKey={`${navigationKey}:${settingsPage}:${downloadPage}`}
            onNotify={notify}
          />
          {launcherMedia.musicUi}
          <header
            className="titlebar"
            data-tauri-drag-region
            onDoubleClick={(e) => {
              if (
                native &&
                !launcher.prefs.appearance.lock_window_size &&
                (e.target as HTMLElement).hasAttribute("data-tauri-drag-region")
              )
                getCurrentWindow().toggleMaximize();
            }}
          >
            {screen === "home" ? (
              <>
                <div className="brand" data-tauri-drag-region>
                  {launcher.prefs.title.mode === "none" ? null : launcher.prefs
                      .title.mode === "text" ? (
                    <span className="wordmark">
                      {launcher.prefs.title.text}
                    </span>
                  ) : launcher.prefs.title.mode === "image" &&
                    launcherMedia.title ? (
                    <img
                      className="launcher-title-image"
                      src={mediaUrl("titles", launcherMedia.title.id)}
                      alt=""
                    />
                  ) : (
                    <>
                      <span className="wordmark pcl-wordmark">PCL</span>
                      <img className="rh-mark" src={rhMark} alt="RH" />
                    </>
                  )}
                </div>
                <nav aria-label={t("main.navigation")}>
                  {[
                    { id: "launch", text: t("nav.launch"), icon: Play },
                    { id: "download", text: t("nav.download"), icon: Download },
                    {
                      id: "settings",
                      text: t("nav.settings"),
                      icon: SettingsIcon,
                    },
                  ]
                    .filter(
                      (n) =>
                        n.id === "launch" ||
                        !launcher.isHidden(`main.${n.id}` as LauncherMenuId),
                    )
                    .map((n) => (
                      <button
                        key={n.id}
                        className={tab === n.id ? "active" : ""}
                        onClick={() => {
                          setTab(n.id);
                          if (n.id === "settings" && data)
                            setDraft(data.settings);
                        }}
                      >
                        <n.icon size={17} />
                        {n.text}
                      </button>
                    ))}
                  {!launcher.isHidden("main.tools") && (
                    <div className="rh-code-nav">
                      <button
                        className={
                          tab === "tools" &&
                          toolsPage !== "extensions" &&
                          toolsPage !== "marketplace"
                            ? "active"
                            : ""
                        }
                        onClick={() => {
                          setTab("tools");
                          setToolsPage("projects");
                        }}
                      >
                        <img src={rhMark} alt="RH" />
                        Code
                      </button>
                      <button
                        className={
                          "rh-plugin-nav " +
                          (tab === "tools" &&
                          (toolsPage === "extensions" ||
                            toolsPage === "marketplace")
                            ? "active"
                            : "")
                        }
                        aria-label={t("experimental.extensions")}
                        onClick={() => {
                          setTab("tools");
                          setToolsPage("extensions");
                        }}
                      >
                        <Puzzle size={16} />
                      </button>
                    </div>
                  )}
                </nav>
              </>
            ) : (
              <button
                className="back-heading"
                onClick={() =>
                  setScreen(
                    screen === "resource"
                      ? resourceOrigin
                      : screen === "tasks"
                        ? taskOrigin
                        : "home",
                  )
                }
              >
                <ArrowLeft size={20} />
                <span>
                  {screen === "versions"
                    ? t("main.instanceSelection")
                    : screen === "resource"
                      ? t("main.resourceHeading", {
                          title: resource?.title || "",
                        })
                      : screen === "tasks"
                        ? t("main.tasks")
                        : t("main.instanceHeading", { id: selected?.id || "" })}
                </span>
              </button>
            )}
            <div className="window-buttons">
              <button
                title={t("main.minimize")}
                aria-label={t("main.minimize")}
                onClick={() => native && getCurrentWindow().minimize()}
              >
                <Minus size={18} />
              </button>
              <button
                title={t("main.close")}
                aria-label={t("main.close")}
                onClick={() => native && getCurrentWindow().close()}
              >
                <X size={18} />
              </button>
            </div>
          </header>
          {error ? (
            <div className="initial-error">
              <TriangleAlert />
              <h2>{t("main.readError")}</h2>
              <p>{error}</p>
              <button className="ce-button" onClick={load}>
                {t("main.reload")}
              </button>
            </div>
          ) : !data ? (
            <div className="loading">
              {launcher.view.revision &&
                launcher.prefs.appearance.show_logo && (
                  <span className="wordmark">PCL RH</span>
                )}
              <LoaderCircle className="spin" />
              {t("main.loading")}
            </div>
          ) : (
            <div
              className={`body-layout ${screen === "home" && tab === "tools" ? "rh-code-space" : ""}`}
            >
              <aside
                className={
                  screen === "home" && tab === "launch"
                    ? "launch-sidebar"
                    : "section-sidebar"
                }
              >
                {screen === "tasks" ? (
                  <TaskStatistics
                    status={aggregateStatus}
                    speed={speed}
                    tasks={downloadStatuses}
                  />
                ) : screen === "resource" ? null : screen === "versions" ? (
                  <>
                    <div className="section-label">{t("main.folders")}</div>
                    {data.roots?.map((root) => (
                      <div
                        className="ce-root-entry"
                        key={root.id}
                        onContextMenu={(event) => {
                          event.preventDefault();
                          rootOptions(root.id, event.clientX, event.clientY);
                        }}
                      >
                        <button
                          className={`folder-entry ${root.id === rootId ? "selected" : ""} ${root.available ? "" : "unavailable"}`}
                          title={
                            root.error
                              ? `${root.path}\n${root.error}`
                              : root.path
                          }
                          aria-pressed={root.id === rootId}
                          disabled={rootWorking}
                          onClick={() =>
                            void rootAction("root_select", { id: root.id })
                          }
                        >
                          <strong>{root.name}</strong>
                          <small>{root.path}</small>
                          {!root.available && (
                            <small>{t("main.unavailable")}</small>
                          )}
                        </button>
                        <button
                          className="icon-button ce-root-options"
                          aria-label={t("folders.manageNamed", {
                            name: root.name,
                          })}
                          title={t("main.manageFolder")}
                          disabled={rootWorking}
                          onClick={(event) => {
                            event.stopPropagation();
                            const rect =
                              event.currentTarget.getBoundingClientRect();
                            rootOptions(root.id, rect.right, rect.top);
                          }}
                        >
                          <MoreHorizontal size={17} />
                        </button>
                      </div>
                    ))}
                    <div className="section-label folder-label">
                      {t("main.addImport")}
                    </div>
                    <button
                      className="side-item"
                      disabled={rootWorking}
                      onClick={() => void addRoot()}
                    >
                      <FolderInput size={19} />
                      <span>{t("main.addFolder")}</span>
                    </button>
                    <button
                      className="side-item"
                      disabled={
                        !native ||
                        archiveAdmission.current.packBlocked ||
                        !archiveAdmission.current.packAvailable ||
                        !!instanceImport
                      }
                      title={
                        !native
                          ? t("main.importDesktop")
                          : t("main.importOwnZip")
                      }
                      onClick={openInstanceImport}
                    >
                      <PackagePlus size={19} />
                      <span>{t("main.importPack")}</span>
                    </button>
                  </>
                ) : screen === "instance" ? (
                  menu(
                    instancePages.map((p) => ({
                      ...p,
                      label: t(p.labelKey),
                      group: p.groupKey ? t(p.groupKey) : undefined,
                      icon:
                        p.id === "settings"
                          ? SettingsIcon
                          : p.id === "modify"
                            ? Wrench
                            : p.icon || Box,
                      disabled: p.unavailable && p.id !== "server",
                    })),
                    instancePage,
                    setInstancePage,
                    "instance",
                  )
                ) : tab === "launch" ? (
                  <>
                    {profileList ? (
                      <div className="ce-profile-list">
                        <button
                          className="ce-profile-list-row"
                          disabled={!!busy}
                          onClick={() => {
                            if (native)
                              void authAction("auth_select", { id: null });
                            setProfileList(false);
                          }}
                        >
                          <span
                            className="ce-profile-list-avatar"
                            style={{
                              backgroundImage: `url(${defaultSkin}),url(${defaultSkin})`,
                            }}
                          />
                          <span>
                            <strong>{data.settings.player}</strong>
                            <small>{t("accounts.offlineAuth")}</small>
                          </span>
                        </button>
                        {data.auth.accounts.map((account) => (
                          <button
                            key={account.profile.id}
                            className="ce-profile-list-row"
                            disabled={!!busy}
                            onClick={() => {
                              if (native)
                                void authAction("auth_select", {
                                  id: account.profile.id,
                                });
                              setProfileList(false);
                            }}
                          >
                            <AccountAvatar
                              image={
                                accountPresentation.images[account.profile.id]
                                  ?.avatar_png
                              }
                            />
                            <span>
                              <strong>{account.profile.name}</strong>
                              <small>{t("accounts.microsoftAuth")}</small>
                            </span>
                          </button>
                        ))}
                        <button
                          className="ce-button ce-profile-create"
                          title={t("accounts.newProfile")}
                          aria-label={t("accounts.newProfile")}
                          onClick={() => {
                            setAccountType("");
                            setDialog("account-type");
                          }}
                        >
                          <UserPlus size={16} />
                        </button>
                      </div>
                    ) : data.status.stage !== "preparing" || !boundGame ? (
                      <div className="ce-profile-current">
                        <button
                          className="account-panel"
                          onClick={openAccountDialog}
                          aria-label={t("accounts.manage")}
                        >
                          <AccountAvatar
                            className="sidebar"
                            image={
                              activeAccount
                                ? accountPresentation.images[
                                    activeAccount.profile.id
                                  ]?.avatar_png
                                : null
                            }
                          />
                          <span className="account-heading">
                            {activeAccount?.profile.name ||
                              data.settings.player}
                          </span>
                          <span className="account-kind">
                            {activeAccount
                              ? t("accounts.microsoftAuth")
                              : t("accounts.offlineLogin")}
                          </span>
                        </button>
                        <div className="ce-profile-actions">
                          <button
                            className="icon-button"
                            title={t("accounts.appearance")}
                            aria-label={t("accounts.appearance")}
                            disabled={!activeAccount}
                            onClick={() =>
                              activeAccount &&
                              openAccountAppearance(activeAccount.profile.id)
                            }
                          >
                            <Shirt size={17} />
                          </button>
                          <button
                            className="icon-button"
                            title={t("accounts.edit")}
                            aria-label={t("accounts.edit")}
                            onClick={openAccountDialog}
                          >
                            <Pencil size={17} />
                          </button>
                          <button
                            className="icon-button"
                            title={t("accounts.selectProfile")}
                            aria-label={t("accounts.selectProfile")}
                            onClick={() => setProfileList(true)}
                          >
                            <Users size={17} />
                          </button>
                        </div>
                      </div>
                    ) : null}
                    {data.status.stage === "preparing" && boundGame && (
                      <div className="ce-launch-preparing">
                        <Pickaxe size={30} />
                        <h2>{t("main.launching")}</h2>
                        <p>{data.status.message}</p>
                        <small>
                          {t("main.authMethod")}
                          {activeAccount
                            ? t("accounts.microsoftAuth")
                            : t("accounts.offlineAuth")}
                        </small>
                        {launcher.prefs.appearance.launch_tips && (
                          <p className="launcher-tip">{t("main.launchTip")}</p>
                        )}
                        <div className="download-progress indeterminate">
                          <i />
                        </div>
                      </div>
                    )}
                    <div className="launch-controls">
                      <button
                        className="launch-button"
                        disabled={
                          !native ||
                          (!boundGame &&
                            (!selected ||
                              downloadBusy ||
                              resourceBusy ||
                              !!inspectionTarget ||
                              !!instanceLogsTarget ||
                              !!failureTarget ||
                              rootWorking ||
                              !rootAvailable ||
                              instanceRecoveryBlocked ||
                              !!busy))
                        }
                        onClick={() =>
                          boundGame
                            ? api("stop_game").catch((e) => notify(String(e)))
                            : launch()
                        }
                      >
                        <span>
                          {boundGame
                            ? data.status.stage === "preparing"
                              ? t("common.cancel")
                              : t("main.stopGame")
                            : t("main.launchGame")}
                        </span>
                        <small>
                          {boundGame
                            ? data.status.message
                            : busy
                              ? t("main.otherGame")
                              : selected?.id || t("main.selectGame")}
                        </small>
                      </button>
                      {!launcher.isHidden("feature.instance_management") && (
                        <div className="launch-secondary">
                          <button
                            className="ce-button"
                            disabled={rootWorking}
                            onClick={() => {
                              setQuery("");
                              setScreen("versions");
                            }}
                          >
                            {t("main.instanceSelection")}
                          </button>
                          <button
                            className="ce-button"
                            disabled={!selected || rootWorking}
                            onClick={instanceSettings}
                          >
                            {t("main.instanceSettings")}
                          </button>
                        </div>
                      )}
                    </div>
                  </>
                ) : tab === "download" ? (
                  menu(downloadItems, downloadPage, setDownloadPage)
                ) : tab === "settings" ? (
                  menu(
                    settingsItems,
                    settingsPage,
                    (id) => {
                      setSettingsPage(id);
                    },
                    "settings",
                  )
                ) : null}
              </aside>
              <main className="content" ref={contentRef}>
                {data.damage_recovery_error && screen !== "tasks" && (
                  <section className="error-banner" role="alert">
                    <TriangleAlert size={17} />
                    <span>{data.damage_recovery_error}</span>
                    <button
                      className="ce-button"
                      disabled={
                        !native ||
                        archiveAdmission.current.blocked ||
                        !rootAvailable
                      }
                      onClick={() => void recoverDamage()}
                    >
                      {t("damage.recover")}
                    </button>
                  </section>
                )}
                {data.import_recovery_error && screen !== "tasks" && (
                  <section className="error-banner" role="alert">
                    <TriangleAlert size={17} />
                    <span>{data.import_recovery_error}</span>
                    <button
                      className="ce-button"
                      disabled={
                        !native ||
                        archiveAdmission.current.blocked ||
                        !rootAvailable
                      }
                      onClick={() => void recoverInstanceArchives("import")}
                    >
                      {t("main.recoverImport")}
                    </button>
                  </section>
                )}
                {data.delete_recovery_error && screen !== "tasks" && (
                  <section className="error-banner" role="alert">
                    <TriangleAlert size={17} />
                    <span>{data.delete_recovery_error}</span>
                    <button
                      className="ce-button"
                      title={
                        data.roots?.find(
                          (root) => root.id === data.delete_recovery_root_id,
                        )?.path
                      }
                      disabled={
                        !native ||
                        archiveAdmission.current.blocked ||
                        !data.delete_recovery_root_id ||
                        !data.roots?.some(
                          (root) =>
                            root.id === data.delete_recovery_root_id &&
                            root.available,
                        )
                      }
                      onClick={() => void recoverInstanceArchives("delete")}
                    >
                      {t("main.recoverDelete")}
                    </button>
                  </section>
                )}
                {data.rename_recovery_error && screen !== "tasks" && (
                  <section className="error-banner" role="alert">
                    <TriangleAlert size={17} />
                    <span>{data.rename_recovery_error}</span>
                    <button
                      className="ce-button"
                      title={
                        data.roots?.find(
                          (root) => root.id === data.rename_recovery_root_id,
                        )?.path
                      }
                      disabled={
                        !native ||
                        !!busy ||
                        downloadBusy ||
                        resourceBusy ||
                        !data.rename_recovery_root_id ||
                        !data.roots?.some(
                          (root) =>
                            root.id === data.rename_recovery_root_id &&
                            root.available,
                        )
                      }
                      onClick={() => void recoverInstanceRename()}
                    >
                      {t("main.recoverRename")}
                    </button>
                  </section>
                )}
                {data.resource_install_recovery_error && screen !== "tasks" && (
                  <section className="error-banner" role="alert">
                    <TriangleAlert size={17} />
                    <span>{data.resource_install_recovery_error}</span>
                    <button
                      className="ce-button"
                      disabled={
                        !native ||
                        archiveAdmission.current.blocked ||
                        !rootAvailable
                      }
                      onClick={() => void recoverResourceInstall()}
                    >
                      {t("main.recoverResource")}
                    </button>
                  </section>
                )}
                {data.reset_recovery_error && screen !== "tasks" && (
                  <section className="error-banner" role="alert">
                    <TriangleAlert size={17} />
                    <span>{data.reset_recovery_error}</span>
                    <button
                      className="ce-button"
                      disabled={
                        !native ||
                        !!busy ||
                        downloadBusy ||
                        resourceBusy ||
                        !rootAvailable
                      }
                      onClick={() => void recoverInstanceReset()}
                    >
                      {t("main.recoverReset")}
                    </button>
                  </section>
                )}
                <div hidden={screen !== "home" || tab !== "download"}>
                  <DownloadPanel
                    visible={screen === "home" && tab === "download"}
                    section={downloadPage}
                    compatibility={resourceBrowse}
                    api={api}
                    rootId={rootId}
                    rootAvailable={rootAvailable && !rootWorking}
                    native={native && !tasks.closing}
                    installed={data.instances}
                    gameBusy={
                      !!busy ||
                      resourceBusy ||
                      instanceRecoveryBlocked ||
                      queueFull
                    }
                    onResourceDetails={showResource}
                    onTaskStart={showInstanceTask}
                  />
                </div>
                <RHCodeHost
                  launcherSurfaces={launcherSurfaces}
                  onPluginConfiguration={setDisabledProductPlugins}
                  visible={screen === "home" && tab === "tools"}
                  api={api}
                  native={native && !tasks.closing}
                  section={
                    toolsPage === "extensions" || toolsPage === "marketplace"
                      ? "plugins"
                      : "code"
                  }
                  onNavigate={navigateExperimental}
                  porterIntake={porterIntake}
                  onPorterIntakeConsumed={() => setPorterIntake(null)}
                />
                <div
                  key={`${rootKey}:${screen}:${tab}:${instancePage}:${settingsPage}`}
                  hidden={screen === "home" && tab === "tools"}
                  className="ce-page-enter ce-main-page"
                >
                  {screen === "home" && tab === "launch" && (
                    <ExperimentalCards
                      api={api}
                      native={native && !tasks.closing}
                      slot="home.banner"
                      onNavigate={navigateExperimental}
                    />
                  )}

                  {screen === "resource" && resource ? (
                    <ResourceDetails
                      key={`${rootKey}:${resource.source}:${resource.project_id || resource.local_path}`}
                      api={rootApi}
                      resource={resource}
                      occupiedNames={
                        data?.instances.map((instance) => instance.id) || []
                      }
                      onNotify={notify}
                      scopeKey={rootId || ""}
                      selectedInstance={selected || null}
                      native={native && !tasks.closing}
                      disabled={
                        !!busy ||
                        resourceBusy ||
                        rootWorking ||
                        !rootAvailable ||
                        instanceRecoveryBlocked ||
                        queueFull
                      }
                      saveDisabled={queueFull || tasks.closing}
                      saveStartDisabled={!!busy || queueFull || tasks.closing}
                      saveStartDisabledReason={
                        busy
                          ? t("task.gameBusy")
                          : queueFull
                            ? t("task.queueFull")
                            : undefined
                      }
                      onTaskStart={showInstanceTask}
                      onResourceDetails={(next) =>
                        showResource(next, resourceOrigin)
                      }
                      onPorter={
                        launcher.isHidden("tools.porter")
                          ? undefined
                          : (intake) => {
                              setPorterIntake({
                                ...intake,
                                id: crypto.randomUUID(),
                              });
                              setScreen("home");
                              setTab("tools");
                              setToolsPage("porter");
                            }
                      }
                    />
                  ) : screen === "tasks" ? (
                    <>
                      {tasks.error && (
                        <div className="error-banner" role="alert">
                          <TriangleAlert size={17} />
                          <span>
                            {t("common.serviceError", { error: tasks.error })}
                          </span>
                          <button
                            className="ce-button"
                            disabled={tasks.closing}
                            onClick={() => void tasks.refresh()}
                          >
                            {t("task.retry")}
                          </button>
                        </div>
                      )}
                      <TaskManager
                        api={api}
                        status={downloadStatus}
                        tasks={visibleTasks}
                        native={native && !tasks.closing}
                        onNotify={notify}
                        onStatusChange={acceptDownloadStatus}
                        onCancelled={finishTaskCancellation}
                        onDismiss={dismissTask}
                      />
                    </>
                  ) : screen === "versions" ? (
                    <>
                      <InstanceTrash
                        api={rootApi}
                        rootId={rootId || ""}
                        scopeKey={`${rootKey}:${data.settings.revision || ""}`}
                        native={native}
                        disabled={
                          archiveAdmission.current.blocked ||
                          !archiveAdmission.current.rootAvailable
                        }
                        onTaskStart={showInstanceTask}
                        refreshKey={`${data.settings.revision || ""}:${taskView.revision}`}
                      />
                      {data.config_warning && (
                        <div className="error-banner" role="alert">
                          <TriangleAlert size={17} />
                          <span>{data.config_warning}</span>
                        </div>
                      )}
                      {(data.scan_error ||
                        selectedRoot?.error ||
                        selectedRoot?.available === false) && (
                        <div className="error-banner" role="alert">
                          <TriangleAlert size={17} />
                          <span>
                            {data.scan_error ||
                              selectedRoot?.error ||
                              t("main.unavailable")}
                            {selectedRoot?.available === false && (
                              <>
                                {" · "}
                                {t(
                                  (data.roots?.length || 0) <= 1
                                    ? "folders.unavailableLastHelp"
                                    : "folders.unavailableHelp",
                                )}
                              </>
                            )}
                          </span>
                        </div>
                      )}
                      {!!data.scan_issues?.length && (
                        <div className="auth-notice" role="status">
                          <p>{t("main.partialScan")}</p>
                          {data.scan_issues.map((issue, index) => (
                            <p key={`${issue.id}:${index}`}>
                              {issue.id}：{issue.message}
                            </p>
                          ))}
                        </div>
                      )}
                      <InstanceSelection
                        key={rootKey}
                        instances={data.instances}
                        query={query}
                        setQuery={setQuery}
                        disabled={rootWorking}
                        onPick={pick}
                      />
                    </>
                  ) : screen === "instance" && selected ? (
                    <InstancePanel
                      key={`${rootKey}:${selected.id}`}
                      overviewContributions={
                        <>
                          <LauncherSurfaceSlot
                            port={launcherSurfaces}
                            slot="instance.overview.tools"
                            host={instanceSurfaceHost}
                          />
                          <LauncherSurfaceSlot
                            port={launcherSurfaces}
                            slot="instance.overview.copy"
                            host={instanceSurfaceHost}
                          />
                        </>
                      }
                      resourceRefreshKey={`${rootKey}:${resourceScanGeneration}`}
                      instance={selected}
                      section={instancePage}
                      settings={data.settings}
                      api={rootApi}
                      native={native}
                      onTaskStart={showInstanceTask}
                      occupiedNames={data.instances.map(
                        (instance) => instance.id,
                      )}
                      onSave={save}
                      onOpen={open}
                      onTestLaunch={launch}
                      onInspect={inspect}
                      onLogs={showInstanceLogs}
                      logsDisabled={instanceLogsDisabled}
                      onNotify={notify}
                      onMetadataChange={(id: string, meta: MetaView) => {
                        if (contextKey.current !== rootKey) return;
                        setData((current) => {
                          if (
                            !current ||
                            `${current.settings.root_id || ""}:${current.settings.root}` !==
                              rootKey
                          )
                            return current;
                          const { revision, ...metadata } = meta;
                          return {
                            ...current,
                            instances: current.instances.map((instance) =>
                              instance.id === id
                                ? {
                                    ...instance,
                                    metadata,
                                    metadata_revision: revision,
                                  }
                                : instance,
                            ),
                          };
                        });
                      }}
                      onResourceDetails={(r) =>
                        showResource(
                          {
                            project_id: "",
                            title: r.name,
                            description: r.description || "",
                            icon_url: r.icon || null,
                            categories: [],
                            display_categories: [],
                            versions: [selected.minecraft_version],
                            source: "local",
                            file_name: r.file_name,
                            local_path: r.path,
                            local_version: r.version,
                            enabled: r.enabled,
                            local_minecraft_version: selected.minecraft_version,
                            local_loader: selected.loader,
                            project_type: r.kind === "mods" ? "mod" : r.kind,
                          },
                          "instance",
                        )
                      }
                      disabled={rootOccupied || rootWorking}
                      mutationDisabled={
                        !!busy ||
                        downloadBusy ||
                        resourceBusy ||
                        rootWorking ||
                        !rootAvailable ||
                        instanceRecoveryBlocked
                      }
                    />
                  ) : screen === "home" ? (
                    <>
                      {tab === "launch" &&
                        processInRoot &&
                        data.status.stage === "error" && (
                          <>
                            <button
                              className="error-banner"
                              onClick={diagnoseFailure}
                              title={t("failure.open")}
                              disabled={
                                !native ||
                                rootWorking ||
                                !rootAvailable ||
                                tasks.closing
                              }
                            >
                              <TriangleAlert size={18} />
                              <span>{data.status.message}</span>
                              <ChevronRight size={18} />
                            </button>
                            {selected?.id === data.status.version && (
                              <div className="ce-actions">
                                <button
                                  className="ce-button"
                                  onClick={inspect}
                                  disabled={
                                    !native ||
                                    !!busy ||
                                    downloadBusy ||
                                    resourceBusy ||
                                    rootWorking ||
                                    !rootAvailable ||
                                    instanceRecoveryBlocked ||
                                    tasks.closing
                                  }
                                >
                                  {t("inspection.open")}
                                </button>
                              </div>
                            )}
                          </>
                        )}
                      {tab === "launch" && (
                        <ExperimentalCards
                          api={api}
                          native={native && !tasks.closing}
                          slot="home.quick-actions"
                          onNavigate={navigateExperimental}
                        />
                      )}
                      {tab === "launch" && (
                        <ExperimentalCards
                          api={api}
                          native={native && !tasks.closing}
                          slot="home.secondary"
                          onNavigate={navigateExperimental}
                        />
                      )}
                      {tab === "launch" && launcherMedia.homeUi}
                      {tab === "launch" && (
                        <LauncherSurfaceSlot
                          port={launcherSurfaces}
                          slot="home.announcements"
                          host={launcherSurfaceHost}
                        />
                      )}
                      {tab === "settings" &&
                        settingsPage !== "toolbox" &&
                        (["launch", "java"].includes(settingsPage) ? (
                          <SettingsPanel
                            section={settingsPage}
                            settings={data.settings}
                            key={rootKey}
                            api={rootApi}
                            native={native}
                            onSave={save}
                            onRefresh={load}
                            onJavaTask={(id, reveal) => {
                              if (reveal) void showInstanceTask(id);
                              else {
                                tasks.track(id);
                                void tasks.refresh();
                              }
                            }}
                            downloadDisabled={
                              !!busy ||
                              queueFull ||
                              resourceBusy ||
                              rootWorking ||
                              instanceRecoveryBlocked ||
                              tasks.closing
                            }
                            disabled={
                              !!busy ||
                              downloadBusy ||
                              resourceBusy ||
                              rootWorking ||
                              instanceRecoveryBlocked
                            }
                            onInstances={instanceSettings}
                            javaRecommendation={
                              javaRecommendation?.rootKey === rootKey &&
                              javaRecommendation.id === selected?.id
                                ? {
                                    name: javaRecommendation.id,
                                    major: javaRecommendation.major,
                                  }
                                : undefined
                            }
                            onInstanceJava={() => {
                              setInspectionTarget(null);
                              setScreen("instance");
                              setInstancePage("settings");
                            }}
                            showInstanceSettings={
                              !launcher.isHidden("feature.instance_management")
                            }
                            launcherPreferences={launcher.view}
                            onLauncherPatch={launcher.patch}
                            launcherBusy={launcher.busy}
                            exitAfterLaunchAvailable={native}
                            onNotify={notify}
                          />
                        ) : (
                          <ExtraSettings
                            key={`${rootKey}:${settingsPage}`}
                            section={settingsPage}
                            settings={data.settings}
                            api={rootApi}
                            initialLog={
                              logEntry?.rootKey === rootKey
                                ? {
                                    name: logEntry.name,
                                    requestKey: logEntry.requestKey,
                                  }
                                : undefined
                            }
                            onOpen={open}
                            onNotify={notify}
                            preferences={launcher.view}
                            onPatch={launcher.patch}
                            preferenceBusy={launcher.busy}
                            supportedEffects={[
                              ...launcher.effects,
                              ...launcherMedia.effects,
                              ...launcherLocal.effects,
                              ...launcherUpdates.effects,
                              ...minecraftNotices.effects,
                              ...launcherDiscovery.effects,
                              ...launcherSurfaceEffects,
                            ]}
                            onLauncherAction={(action) =>
                              launcherMedia.handles(action)
                                ? launcherMedia.action(action)
                                : launcherLocal.handles(action)
                                  ? launcherLocal.action(action)
                                  : launcher.action(action)
                            }
                            revealHidden={launcher.revealed}
                            fontFamilies={launcherMedia.fonts}
                            native={native}
                          />
                        ))}
                      {tab === "settings" && settingsPluginSlot && (
                        <ExperimentalCards
                          api={api}
                          native={native && !tasks.closing}
                          slot={settingsPluginSlot}
                          onNavigate={navigateExperimental}
                        />
                      )}
                      {tab === "settings" && settingsPage === "toolbox" && (
                        <Toolbox
                          onOpen={open}
                          onExperimentalNavigate={navigateExperimental}
                          api={api}
                          onTaskStart={showInstanceTask}
                          native={native && !tasks.closing}
                          busy={launcher.busy}
                          networkSubmissionDisabled={
                            !!busy || queueFull || tasks.closing
                          }
                          networkSubmissionReason={
                            busy
                              ? t("task.gameBusy")
                              : queueFull
                                ? t("task.queueFull")
                                : undefined
                          }
                          onTool={launcherLocal.tool}
                        />
                      )}
                    </>
                  ) : null}
                </div>
              </main>
            </div>
          )}
          {visibleTasks.some(taskHasFloatingEntry) && screen !== "tasks" && (
            <button
              className="ce-task-entry"
              title={t("task.listCount", {
                count: visibleTasks.filter(taskHasFloatingEntry).length,
              })}
              aria-label={t("task.listCount", {
                count: visibleTasks.filter(taskHasFloatingEntry).length,
              })}
              onClick={showTasks}
            >
              <Download size={23} />
              <span className="ce-task-count">
                {formatNumber(visibleTasks.filter(taskHasFloatingEntry).length)}
              </span>
            </button>
          )}
          {data &&
            failureRun &&
            failureTarget &&
            failureTarget.rootKey === rootKey &&
            failureTarget.key === failureKey &&
            failureTarget.epoch === navigationEpoch && (
              <LaunchFailure
                expected={failureRun}
                scopeKey={`${rootKey}:${failureTarget.epoch}:${disabledProductPlugins.includes("launcher.diagnostics.analysis")}`}
                api={rootApi}
                native={native}
                disabled={rootWorking || !rootAvailable || tasks.closing}
                canInspect={
                  !!selected &&
                  selected.id === failureRun.version &&
                  !busy &&
                  !downloadBusy &&
                  !resourceBusy &&
                  !rootWorking &&
                  rootAvailable &&
                  !instanceRecoveryBlocked &&
                  !tasks.closing
                }
                onClose={() =>
                  setFailureTarget((current) =>
                    current === failureTarget ? null : current,
                  )
                }
                onInspect={() => {
                  if (
                    contextKey.current !== failureTarget.rootKey ||
                    currentFailureKey.current !== failureTarget.key ||
                    taskNavigation.current.epoch !== failureTarget.epoch ||
                    viewContext.current.selected !== failureRun.version
                  )
                    return;
                  inspect();
                }}
                onLogs={(name) => {
                  if (
                    !name ||
                    contextKey.current !== failureTarget.rootKey ||
                    currentFailureKey.current !== failureTarget.key ||
                    taskNavigation.current.epoch !== failureTarget.epoch
                  )
                    return;
                  // A diagnostic opens its exact log once. Returning to general
                  // settings later must not resurrect this selection request.
                  setLogEntry({
                    rootKey,
                    name,
                    requestKey: String(++logEntrySequence.current),
                  });
                  setScreen("home");
                  setTab("settings");
                  setSettingsPage("logs");
                }}
              />
            )}
          {data &&
            instanceLogsTarget &&
            instanceLogsTarget.rootKey === rootKey &&
            instanceLogsTarget.id === selected?.id &&
            instanceLogsTarget.epoch === navigationEpoch && (
              <InstanceLogs
                key={`${instanceLogsTarget.rootKey}:${instanceLogsTarget.id}:${instanceLogsTarget.epoch}`}
                id={instanceLogsTarget.id}
                scopeKey={`${rootKey}:${instanceLogsTarget.epoch}`}
                revision={data.settings.revision || ""}
                api={rootApi}
                native={native}
                disabled={instanceLogsDisabled}
                language={launcher.prefs.localization.language}
                region={launcher.prefs.localization.region}
                onClose={() =>
                  setInstanceLogsTarget((current) =>
                    current === instanceLogsTarget ? null : current,
                  )
                }
              />
            )}
          {data &&
            inspectionTarget &&
            inspectionTarget.rootKey === rootKey &&
            inspectionTarget.id === selected?.id &&
            inspectionTarget.epoch === navigationEpoch && (
              <LaunchInspection
                key={`${inspectionTarget.rootKey}:${inspectionTarget.id}:${inspectionTarget.epoch}`}
                id={inspectionTarget.id}
                scopeKey={`${rootKey}:${data.settings.revision || ""}`}
                api={rootApi}
                revision={data.settings.revision || ""}
                native={native}
                disabled={
                  !!busy ||
                  downloadBusy ||
                  resourceBusy ||
                  rootWorking ||
                  !rootAvailable ||
                  instanceRecoveryBlocked ||
                  tasks.closing
                }
                onClose={() =>
                  setInspectionTarget((current) =>
                    current === inspectionTarget ? null : current,
                  )
                }
                onManageJava={(major) => {
                  if (
                    contextKey.current !== inspectionTarget.rootKey ||
                    viewContext.current.selected !== inspectionTarget.id ||
                    taskNavigation.current.epoch !== inspectionTarget.epoch
                  )
                    return;
                  setJavaRecommendation({
                    rootKey,
                    id: inspectionTarget.id,
                    major,
                  });
                  setInspectionTarget(null);
                  setTab("settings");
                  setSettingsPage("java");
                  setScreen("home");
                }}
                onTaskStart={(id, reveal) => {
                  // Submission owns the task even after this modal closes.
                  // Only its original navigation may open the task page.
                  tasks.track(id);
                  if (
                    reveal &&
                    contextKey.current === inspectionTarget.rootKey &&
                    viewContext.current.selected === inspectionTarget.id &&
                    taskNavigation.current.epoch === inspectionTarget.epoch
                  )
                    void showInstanceTask(id);
                  else void tasks.refresh();
                }}
              />
            )}
          {data &&
            instanceImport &&
            instanceImport.rootKey === rootKey &&
            screen === "versions" &&
            instanceImport.epoch === taskNavigation.current.epoch && (
              <InstanceImport
                key={`${instanceImport.rootKey}:${instanceImport.epoch}`}
                api={rootApi}
                scopeKey={rootId || data.settings.root}
                native={native}
                disabled={
                  archiveAdmission.current.packBlocked ||
                  !archiveAdmission.current.packAvailable
                }
                occupiedNames={data.instances.map((instance) => instance.id)}
                onTaskStart={showInstanceTask}
                onNotify={notify}
                onClose={() =>
                  setInstanceImport((current) =>
                    current === instanceImport ? null : current,
                  )
                }
              />
            )}
          {rootMenu && menuRoot && (
            <div
              className="ce-root-menu"
              role="menu"
              aria-label={t("folders.manageNamed", { name: menuRoot.name })}
              style={{ left: rootMenu.x, top: rootMenu.y }}
              onClick={(event) => event.stopPropagation()}
            >
              <button
                role="menuitem"
                onClick={() => editRoot(menuRoot, "rename")}
              >
                <Pencil size={16} />
                {t("folders.rename")}
              </button>
              <button
                role="menuitem"
                disabled={rootWorking || menuRootIndex <= 0}
                onClick={() =>
                  void rootAction("root_update", {
                    id: menuRoot.id,
                    position: menuRootIndex - 1,
                  })
                }
              >
                <ArrowUp size={16} />
                {t("folders.up")}
              </button>
              <button
                role="menuitem"
                disabled={
                  rootWorking || menuRootIndex >= (data?.roots?.length || 0) - 1
                }
                onClick={() =>
                  void rootAction("root_update", {
                    id: menuRoot.id,
                    position: menuRootIndex + 1,
                  })
                }
              >
                <ArrowDown size={16} />
                {t("folders.down")}
              </button>
              <button
                role="menuitem"
                disabled={
                  rootWorking ||
                  (data?.roots?.length || 0) <= 1 ||
                  menuRootOccupied
                }
                title={
                  menuRootOccupied
                    ? t("folders.occupied")
                    : (data?.roots?.length || 0) <= 1
                      ? t("folders.lastRoot")
                      : undefined
                }
                onClick={() => editRoot(menuRoot, "remove")}
              >
                <X size={16} />
                {t("folders.remove")}
              </button>
            </div>
          )}
          {rootDialog && (
            <div
              className="modal-shade rd-name-shade"
              onMouseDown={(event) => {
                if (event.target === event.currentTarget && !rootWorking)
                  setRootDialog(null);
              }}
            >
              <form
                className="rd-name-dialog ce-root-dialog"
                role="dialog"
                aria-modal="true"
                aria-labelledby="ce-root-dialog-title"
                onSubmit={(event) => {
                  event.preventDefault();
                  if (rootDialog.mode === "rename" && !rootName.trim()) {
                    setRootError(t("folders.nameRequired"));
                    return;
                  }
                  void rootAction(
                    rootDialog.mode === "rename"
                      ? "root_update"
                      : "root_remove",
                    rootDialog.mode === "rename"
                      ? { id: rootDialog.root.id, name: rootName.trim() }
                      : { id: rootDialog.root.id },
                  );
                }}
              >
                <h2 id="ce-root-dialog-title">
                  {rootDialog.mode === "rename"
                    ? t("folders.renameFolder")
                    : t("folders.remove")}
                </h2>
                {rootDialog.mode === "rename" ? (
                  <input
                    className="ce-field"
                    aria-label={t("folders.name")}
                    value={rootName}
                    maxLength={128}
                    disabled={rootWorking}
                    onChange={(event) => setRootName(event.target.value)}
                  />
                ) : (
                  <p>
                    {t(
                      rootDialog.root.available
                        ? "folders.removeHelp"
                        : "folders.removeUnavailableHelp",
                      { name: rootDialog.root.name },
                    )}
                  </p>
                )}
                {rootError && (
                  <p className="rd-name-error" role="alert">
                    {rootError}
                  </p>
                )}
                <div className="rd-name-actions">
                  <button
                    className="ce-button"
                    type="submit"
                    disabled={rootWorking}
                  >
                    {rootWorking
                      ? t("main.saving")
                      : rootDialog.mode === "rename"
                        ? t("main.confirm")
                        : t("main.remove")}
                  </button>
                  <button
                    className="ce-button"
                    type="button"
                    disabled={rootWorking}
                    onClick={() => setRootDialog(null)}
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              </form>
            </div>
          )}
          {dialog === "account-type" && (
            <div
              className="modal-shade"
              onMouseDown={(e) => {
                if (e.target === e.currentTarget) setDialog(null);
              }}
            >
              <section
                className="ce-account-type-modal"
                role="dialog"
                aria-modal="true"
                aria-label={t("accounts.chooseType")}
              >
                <h2>{t("accounts.chooseType")}</h2>
                <div className="ce-account-types">
                  {[
                    {
                      id: "premium",
                      name: t("accounts.microsoftAuth"),
                      icon: ShieldCheck,
                    },
                    {
                      id: "third-party",
                      name: t("accounts.thirdParty"),
                      icon: Network,
                    },
                    {
                      id: "offline",
                      name: t("accounts.offlineAuth"),
                      icon: Unplug,
                    },
                  ].map((item) => (
                    <button
                      key={item.id}
                      className={accountType === item.id ? "selected" : ""}
                      disabled={item.id === "third-party"}
                      title={
                        item.id === "third-party"
                          ? t("accounts.thirdPartyUnavailable")
                          : undefined
                      }
                      onClick={() => setAccountType(item.id)}
                    >
                      <item.icon size={24} />
                      <span>{item.name}</span>
                    </button>
                  ))}
                </div>
                <div className="ce-account-type-footer">
                  <button
                    className="ce-button"
                    disabled={!accountType}
                    onClick={() => setDialog("accounts")}
                  >
                    {t("main.continue")}
                  </button>
                  <button className="ce-button" onClick={() => setDialog(null)}>
                    {t("common.cancel")}
                  </button>
                </div>
              </section>
            </div>
          )}
          {dialog === "accounts" && data && (
            <AccountDialog
              mode={accountType === "offline" ? "offline" : "premium"}
              player={data.settings.player}
              auth={data.auth}
              native={native}
              busy={!!busy}
              working={authWorking}
              error={authError}
              images={accountPresentation.images}
              assist={accountPresentation.assist}
              remember={remember}
              onRemember={setRemember}
              onAction={authAction}
              onOfflineSave={saveOfflineAccount}
              onAppearance={openAccountAppearance}
              onBack={() => {
                setAuthError("");
                setDialog("account-type");
              }}
              onClose={closeAccountDialog}
            />
          )}
          {dialog === "account-appearance" &&
            appearanceId &&
            data?.auth.accounts.some((a) => a.profile.id === appearanceId) && (
              <AccountAppearanceDialog
                name={
                  data.auth.accounts.find((a) => a.profile.id === appearanceId)!
                    .profile.name
                }
                image={accountPresentation.images[appearanceId]}
                onRefresh={() => void accountPresentation.refresh(appearanceId)}
                onClose={() => setDialog(appearanceOrigin)}
              />
            )}
          {launcher.confirmationUi}
          {launcherLocal.ui}
          {toast && (
            <div className="toast" role="status">
              <Info size={17} />
              <span>{toast}</span>
              <button
                aria-label={t("ui.closeNotice")}
                onClick={() => setToast("")}
              >
                <X size={15} />
              </button>
            </div>
          )}
        </div>
      </LauncherNavigationContext.Provider>
    </LauncherFavoritesContext.Provider>
  );
}
createRoot(document.getElementById("root")!).render(<App />);
