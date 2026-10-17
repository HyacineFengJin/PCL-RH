/** Launcher preferences are global application policy, independent of game roots.
 * Keep transport fields aligned with launcher_prefs/model.rs. Runtime support is
 * supplied separately: a stored value alone never enables an unfinished effect.
 */
export type LauncherLocale = "system" | "zh-CN" | "en-US";
export type LauncherMenuId =
  | "main.download"
  | "main.settings"
  | "main.tools"
  | "settings.launch"
  | "settings.java"
  | "settings.manage"
  | "settings.network"
  | "settings.personalize"
  | "settings.language"
  | "settings.misc"
  | "settings.update"
  | "settings.about"
  | "settings.feedback"
  | "settings.logs"
  | "tools.network"
  | "tools.toolbox"
  | "tools.extensions"
  | "tools.marketplace"
  | "tools.maker"
  | "tools.porter"
  | "tools.ai"
  | "instance.modify"
  | "instance.export"
  | "instance.saves"
  | "instance.screenshots"
  | "instance.mods"
  | "instance.resourcepacks"
  | "instance.shaderpacks"
  | "instance.schematics"
  | "instance.server"
  | "feature.instance_management"
  | "feature.mod_updates"
  | "feature.feature_hiding";

export type LauncherPreferences = {
  appearance: {
    opacity_percent: number;
    theme: "system" | "light" | "dark";
    light_palette: "blue";
    dark_palette: "blue";
    show_logo: boolean;
    lock_window_size: boolean;
    launch_tips: boolean;
    advanced_materials: boolean;
    custom_cursor: boolean;
    cursor_style: "accent" | "outline";
    cursor_size: "small" | "standard";
    custom_context_menu: boolean;
    context_menu_density: "comfortable" | "compact";
    global_font: string;
    motd_font: string;
  };
  background: {
    color_overlay: boolean;
    background_asset_id: string;
    music_asset_id: string;
  };
  title: {
    mode: "default" | "none" | "text" | "image";
    text: string;
    image_path: string;
  };
  home: {
    mode: "blank" | "preset" | "local" | "remote";
    preset: "default";
    local_path: string;
    remote_url: string;
  };
  navigation: {
    hidden_menu_ids: LauncherMenuId[];
    f12_reveal_enabled: boolean;
  };
  localization: { language: LauncherLocale; region: LauncherLocale };
  announcement_mode: "all" | "important" | "none";
  animation: { fps_limit: number; speed_percent: number };
  realtime_log_line_limit: number;
  disable_hardware_acceleration: boolean;
  local_diagnostics_enabled: boolean;
  network: {
    proxy_mode: "system" | "none" | "custom";
    custom_proxy_url: string;
    doh_enabled: boolean;
  };
  advanced: {
    debug_mode: boolean;
    artificial_delay_ms: number;
    forbid_download_copy: boolean;
  };
  updates: {
    channel: "stable" | "beta";
    policy: "manual" | "notify" | "download";
  };
  auto_select_installed: boolean;
  management: {
    max_concurrent_transfers: number;
    total_rate_limit_mib_per_second: number;
    clipboard_resource_detection: boolean;
    minecraft_release_notifications: boolean;
    minecraft_snapshot_notifications: boolean;
    download_file_name: "original" | "project_version";
    mod_display_style: "metadata" | "file_name";
    quick_download: "ask" | "last_folder";
  };
  launch_visibility: "always" | "hide_while_game" | "exit_after_launch";
  game_launch: {
    window:
      { mode: "default" } | { mode: "custom"; width: number; height: number };
    ip: "default" | "ipv4" | "ipv6";
    window_title?: string;
  };
};

export type LauncherPreferenceView = {
  preferences: LauncherPreferences;
  revision: string;
  warning: string | null;
  /** Runtime effect failures are visible but do not turn the store readonly. */
  runtime_warning?: string | null;
  hardware_acceleration_restart_required: boolean;
  last_import_backup_path?: string | null;
};
export type LauncherPreferencePatch = {
  [K in keyof LauncherPreferences]?: LauncherPreferences[K] extends object
    ? Partial<LauncherPreferences[K]>
    : LauncherPreferences[K];
};
export type LauncherEffect =
  | "appearance"
  | "fonts"
  | "window_lock"
  | "launch_tips"
  | "title"
  | "home"
  | "background"
  | "music"
  | "navigation"
  | "localization"
  | "animations"
  | "announcements"
  | "log_limit"
  | "hardware_acceleration"
  | "local_diagnostics"
  | "network_proxy"
  | "doh"
  | "download_copy"
  | "download_policy"
  | "clipboard_detection"
  | "mod_display_style"
  | "save_policy"
  | "debug"
  | "artificial_delay"
  | "auto_select_installed"
  | "settings_transfer"
  | "stop_using"
  | "logs"
  | "updates"
  | "minecraft_updates";
export type LauncherAction =
  | "open_background_folder"
  | "refresh_background"
  | "open_music_folder"
  | "refresh_music"
  | "pick_title_image"
  | "pick_home_file"
  | "refresh_home"
  | "export_settings"
  | "import_settings"
  | "stop_using";

export type LauncherPreferencePanelProps = {
  preferences?: LauncherPreferenceView;
  onPatch?: (patch: LauncherPreferencePatch) => Promise<LauncherPreferenceView>;
  preferenceBusy?: boolean;
  supportedEffects?: readonly LauncherEffect[];
  onLauncherAction?: (action: LauncherAction) => Promise<void>;
  fontFamilies?: readonly string[];
  onNotify: (message: string) => void;
};

/** A fresh readonly preview fallback. Never pass its empty revision to native writes. */
export function defaultLauncherPreferenceView(): LauncherPreferenceView {
  return {
    revision: "",
    warning: null,
    hardware_acceleration_restart_required: false,
    preferences: {
      appearance: {
        opacity_percent: 100,
        theme: "light",
        light_palette: "blue",
        dark_palette: "blue",
        show_logo: true,
        lock_window_size: false,
        launch_tips: true,
        advanced_materials: false,
        custom_cursor: true,
        cursor_style: "accent",
        cursor_size: "small",
        custom_context_menu: true,
        context_menu_density: "comfortable",
        global_font: "",
        motd_font: "",
      },
      background: {
        color_overlay: true,
        background_asset_id: "",
        music_asset_id: "",
      },
      title: { mode: "default", text: "", image_path: "" },
      home: {
        mode: "blank",
        preset: "default",
        local_path: "",
        remote_url: "",
      },
      navigation: { hidden_menu_ids: [], f12_reveal_enabled: true },
      localization: { language: "system", region: "system" },
      announcement_mode: "all",
      animation: { fps_limit: 60, speed_percent: 100 },
      realtime_log_line_limit: 1000,
      disable_hardware_acceleration: false,
      local_diagnostics_enabled: false,
      network: {
        proxy_mode: "system",
        custom_proxy_url: "",
        doh_enabled: false,
      },
      advanced: {
        debug_mode: false,
        artificial_delay_ms: 0,
        forbid_download_copy: false,
      },
      updates: { channel: "stable", policy: "manual" },
      management: {
        max_concurrent_transfers: 4,
        total_rate_limit_mib_per_second: 0,
        clipboard_resource_detection: false,
        minecraft_release_notifications: false,
        minecraft_snapshot_notifications: false,
        download_file_name: "original",
        mod_display_style: "metadata",
        quick_download: "ask",
      },
      auto_select_installed: true,
      launch_visibility: "always",
      game_launch: { window: { mode: "default" }, ip: "default" },
    },
  };
}

export function launcherRegion(locale: LauncherLocale): string | undefined {
  return locale === "system" ? undefined : locale;
}

export type LauncherLogRow = {
  name: string;
  path: string;
  modified: number;
  current: boolean;
  bytes: number;
};
export type LauncherLogSelection =
  { kind: "named"; name: string } | { kind: "current" } | { kind: "all" };
export type LauncherLogExportPlan = {
  revision: string;
  fileCount: number;
  bytes: number;
  suggestedName: string;
  format: "text" | "zip";
};
export type LauncherLogExportOutcome = {
  path: string;
  fileCount: number;
  bytes: number;
};
export type LauncherLogClearPlan = {
  revision: string;
  fileCount: number;
  bytes: number;
  names: string[];
  retainedCurrent: string | null;
};
export type LauncherLogClearOutcome = {
  operationId: string;
  fileCount: number;
};
export type LauncherLogRecoveryRow = {
  operationId: string;
  revision: string;
  createdAt: number;
  fileCount: number;
  bytes: number;
  names: string[];
  warnings: string[];
  needsCompletion: boolean;
};
export type LauncherLogRestoreOutcome = {
  operationId: string;
  fileCount: number;
};
