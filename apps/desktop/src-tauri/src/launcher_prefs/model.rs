//! Typed launcher preferences and validation. These values are application
//! policy; storing a value does not imply its runtime feature is implemented.
use pcl_core::launch_options::{IpPreference, LaunchOptions, WindowSize};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

macro_rules! selection {
    ($name:ident { $default:ident $(, $variant:ident)* $(,)? }) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { #[default] $default, $($variant),* }
    };
}

selection!(Theme {
    Light,
    System,
    Dark
});
selection!(Palette { Blue });
selection!(CursorStyle { Accent, Outline });
selection!(CursorSize { Small, Standard });
selection!(ContextMenuDensity {
    Comfortable,
    Compact
});
selection!(DownloadFileName {
    Original,
    ProjectVersion
});
selection!(ModDisplayStyle { Metadata, FileName });
selection!(QuickDownload { Ask, LastFolder });
selection!(TitleMode {
    Default,
    None,
    Text,
    Image
});
selection!(HomeMode {
    Blank,
    Preset,
    Local,
    Remote
});
selection!(HomePreset { Default });
selection!(AnnouncementMode {
    All,
    Important,
    None
});
selection!(ProxyMode {
    System,
    None,
    Custom
});
selection!(UpdateChannel { Stable, Beta });
selection!(UpdatePolicy {
    Manual,
    Notify,
    Download
});
selection!(LaunchVisibility {
    Always,
    HideWhileGame,
    ExitAfterLaunch
});

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

/// Stable IDs use ownership and page names instead of translated labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuId {
    #[serde(rename = "main.download")]
    MainDownload,
    #[serde(rename = "main.settings")]
    MainSettings,
    #[serde(rename = "main.tools")]
    MainTools,
    #[serde(rename = "settings.launch")]
    SettingsLaunch,
    #[serde(rename = "settings.java")]
    SettingsJava,
    #[serde(rename = "settings.manage")]
    SettingsManagement,
    #[serde(rename = "settings.network")]
    SettingsMultiplayer,
    #[serde(rename = "settings.personalize")]
    SettingsPersonalize,
    #[serde(rename = "settings.language")]
    SettingsLanguage,
    #[serde(rename = "settings.misc")]
    SettingsMisc,
    #[serde(rename = "settings.update")]
    SettingsUpdate,
    #[serde(rename = "settings.about")]
    SettingsAbout,
    #[serde(rename = "settings.feedback")]
    SettingsFeedback,
    #[serde(rename = "settings.logs")]
    SettingsLogs,
    #[serde(rename = "tools.network")]
    ToolsMultiplayer,
    #[serde(rename = "tools.toolbox")]
    ToolsToolbox,
    #[serde(rename = "tools.extensions")]
    ToolsExtensions,
    #[serde(rename = "tools.marketplace")]
    ToolsMarketplace,
    #[serde(rename = "tools.maker")]
    ToolsMaker,
    #[serde(rename = "tools.porter")]
    ToolsPorter,
    #[serde(rename = "tools.ai")]
    ToolsAi,
    #[serde(rename = "instance.modify")]
    InstanceModify,
    #[serde(rename = "instance.export")]
    InstanceExport,
    #[serde(rename = "instance.saves")]
    InstanceSaves,
    #[serde(rename = "instance.screenshots")]
    InstanceScreenshots,
    #[serde(rename = "instance.mods")]
    InstanceMods,
    #[serde(rename = "instance.resourcepacks")]
    InstanceResourcepacks,
    #[serde(rename = "instance.shaderpacks")]
    InstanceShaderpacks,
    #[serde(rename = "instance.schematics")]
    InstanceSchematics,
    #[serde(rename = "instance.server")]
    InstanceServers,
    #[serde(rename = "feature.instance_management")]
    FeatureInstanceManagement,
    #[serde(rename = "feature.mod_updates")]
    FeatureModUpdates,
    #[serde(rename = "feature.feature_hiding")]
    FeatureFunctionHiding,
}
impl MenuId {
    pub const ALL: &'static [Self] = &[
        Self::MainDownload,
        Self::MainSettings,
        Self::MainTools,
        Self::SettingsLaunch,
        Self::SettingsJava,
        Self::SettingsManagement,
        Self::SettingsMultiplayer,
        Self::SettingsPersonalize,
        Self::SettingsLanguage,
        Self::SettingsMisc,
        Self::SettingsUpdate,
        Self::SettingsAbout,
        Self::SettingsFeedback,
        Self::SettingsLogs,
        Self::ToolsMultiplayer,
        Self::ToolsToolbox,
        Self::ToolsExtensions,
        Self::ToolsMarketplace,
        Self::ToolsMaker,
        Self::ToolsPorter,
        Self::ToolsAi,
        Self::InstanceModify,
        Self::InstanceExport,
        Self::InstanceSaves,
        Self::InstanceScreenshots,
        Self::InstanceMods,
        Self::InstanceResourcepacks,
        Self::InstanceShaderpacks,
        Self::InstanceSchematics,
        Self::InstanceServers,
        Self::FeatureInstanceManagement,
        Self::FeatureModUpdates,
        Self::FeatureFunctionHiding,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearancePreferences {
    pub opacity_percent: u8,
    pub theme: Theme,
    pub light_palette: Palette,
    pub dark_palette: Palette,
    pub show_logo: bool,
    pub lock_window_size: bool,
    pub launch_tips: bool,
    pub advanced_materials: bool,
    pub custom_cursor: bool,
    pub cursor_style: CursorStyle,
    pub cursor_size: CursorSize,
    pub custom_context_menu: bool,
    pub context_menu_density: ContextMenuDensity,
    /// Empty selects the current launcher CSS font. One family name is accepted.
    pub global_font: String,
    pub motd_font: String,
}
impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            opacity_percent: 100,
            theme: Theme::Light,
            light_palette: Palette::Blue,
            dark_palette: Palette::Blue,
            show_logo: true,
            lock_window_size: false,
            launch_tips: true,
            advanced_materials: false,
            custom_cursor: true,
            cursor_style: CursorStyle::default(),
            cursor_size: CursorSize::default(),
            custom_context_menu: true,
            context_menu_density: ContextMenuDensity::default(),
            global_font: String::new(),
            motd_font: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackgroundPreferences {
    pub color_overlay: bool,
    /// Asset IDs name entries owned by the caller's fixed asset store. They do
    /// not authorize reading arbitrary files; this module never resolves them.
    pub background_asset_id: String,
    pub music_asset_id: String,
}
impl Default for BackgroundPreferences {
    fn default() -> Self {
        Self {
            color_overlay: true,
            background_asset_id: String::new(),
            music_asset_id: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TitlePreferences {
    pub mode: TitleMode,
    pub text: String,
    pub image_path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomePreferences {
    pub mode: HomeMode,
    pub preset: HomePreset,
    pub local_path: String,
    pub remote_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NavigationPreferences {
    pub hidden_menu_ids: Vec<MenuId>,
    /// F12 is the fixed temporary reveal shortcut. Its reveal state is runtime
    /// only, so restarting always returns to the persisted hidden selection.
    pub f12_reveal_enabled: bool,
}

impl Default for NavigationPreferences {
    fn default() -> Self {
        Self {
            hidden_menu_ids: Vec::new(),
            f12_reveal_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalizationPreferences {
    pub language: Locale,
    pub region: Locale,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnimationPreferences {
    pub fps_limit: u16,
    pub speed_percent: u16,
}
impl Default for AnimationPreferences {
    fn default() -> Self {
        Self {
            fps_limit: 60,
            speed_percent: 100,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkPreferences {
    pub proxy_mode: ProxyMode,
    /// Authentication proxies are not supported. Validation rejects userinfo,
    /// queries and fragments before this endpoint can be stored or exported.
    pub custom_proxy_url: String,
    /// Opt-in application DoH with bounded cache and visible system fallback.
    pub doh_enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdvancedPreferences {
    pub debug_mode: bool,
    pub artificial_delay_ms: u16,
    pub forbid_download_copy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagementPreferences {
    pub max_concurrent_transfers: u16,
    pub clipboard_resource_detection: bool,
    pub minecraft_release_notifications: bool,
    pub minecraft_snapshot_notifications: bool,
    pub download_file_name: DownloadFileName,
    pub mod_display_style: ModDisplayStyle,
    pub quick_download: QuickDownload,
    /// Shared artifact bandwidth budget; zero means unlimited. Metadata is
    /// excluded so a slow large file cannot starve catalogs or cancellation.
    pub total_rate_limit_mib_per_second: u32,
}
impl Default for ManagementPreferences {
    fn default() -> Self {
        Self {
            max_concurrent_transfers: 4,
            total_rate_limit_mib_per_second: 0,
            clipboard_resource_detection: false,
            minecraft_release_notifications: false,
            minecraft_snapshot_notifications: false,
            download_file_name: DownloadFileName::Original,
            mod_display_style: ModDisplayStyle::Metadata,
            quick_download: QuickDownload::Ask,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatePreferences {
    pub channel: UpdateChannel,
    pub policy: UpdatePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherPreferences {
    pub appearance: AppearancePreferences,
    pub background: BackgroundPreferences,
    pub title: TitlePreferences,
    pub home: HomePreferences,
    pub navigation: NavigationPreferences,
    pub localization: LocalizationPreferences,
    pub announcement_mode: AnnouncementMode,
    pub animation: AnimationPreferences,
    pub realtime_log_line_limit: u32,
    /// Requires a new application process to affect the WebKit renderer.
    pub disable_hardware_acceleration: bool,
    /// Enables local launcher diagnostics only. This store never transmits logs.
    pub local_diagnostics_enabled: bool,
    pub network: NetworkPreferences,
    pub advanced: AdvancedPreferences,
    pub updates: UpdatePreferences,
    pub management: ManagementPreferences,
    pub auto_select_installed: bool,
    pub launch_visibility: LaunchVisibility,
    /// Global game argv defaults. They never change content-directory isolation.
    pub game_launch: LaunchOptions,
}
impl Default for LauncherPreferences {
    fn default() -> Self {
        Self {
            appearance: AppearancePreferences::default(),
            background: BackgroundPreferences::default(),
            title: TitlePreferences::default(),
            home: HomePreferences::default(),
            navigation: NavigationPreferences {
                hidden_menu_ids: Vec::new(),
                f12_reveal_enabled: true,
            },
            localization: LocalizationPreferences::default(),
            announcement_mode: AnnouncementMode::All,
            animation: AnimationPreferences::default(),
            realtime_log_line_limit: 1000,
            disable_hardware_acceleration: false,
            local_diagnostics_enabled: false,
            network: NetworkPreferences::default(),
            advanced: AdvancedPreferences::default(),
            updates: UpdatePreferences::default(),
            management: ManagementPreferences::default(),
            auto_select_installed: true,
            launch_visibility: LaunchVisibility::Always,
            game_launch: LaunchOptions::default(),
        }
    }
}

// Optional fields intentionally mean a partial edit. Empty strings clear text
// values; omitted fields never overwrite concurrent preferences after refresh.
macro_rules! patch {
    ($name:ident => $target:ident { $($field:ident: $kind:ty),* $(,)? }) => {
        #[derive(Clone, Debug, Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $(pub $field: Option<$kind>),* }
        impl $name {
            pub(crate) fn apply(self, value: &mut $target) {
                $(if let Some(field) = self.$field { value.$field = field; })*
            }
        }
    }
}
patch!(GameLaunchPatch => LaunchOptions { window: WindowSize, ip: IpPreference, window_title: String });
patch!(AppearancePatch => AppearancePreferences {
    opacity_percent: u8, theme: Theme, light_palette: Palette, dark_palette: Palette,
    show_logo: bool, lock_window_size: bool, launch_tips: bool,
    advanced_materials: bool, custom_cursor: bool, cursor_style: CursorStyle,
    cursor_size: CursorSize, custom_context_menu: bool, context_menu_density: ContextMenuDensity,
    global_font: String, motd_font: String,
});
patch!(BackgroundPatch => BackgroundPreferences {
    color_overlay: bool, background_asset_id: String, music_asset_id: String,
});
patch!(TitlePatch => TitlePreferences { mode: TitleMode, text: String, image_path: String });
patch!(HomePatch => HomePreferences {
    mode: HomeMode, preset: HomePreset, local_path: String, remote_url: String,
});
patch!(NavigationPatch => NavigationPreferences {
    hidden_menu_ids: Vec<MenuId>, f12_reveal_enabled: bool,
});
patch!(LocalizationPatch => LocalizationPreferences { language: Locale, region: Locale });
patch!(AnimationPatch => AnimationPreferences { fps_limit: u16, speed_percent: u16 });
patch!(NetworkPatch => NetworkPreferences {
    proxy_mode: ProxyMode, custom_proxy_url: String, doh_enabled: bool,
});
patch!(AdvancedPatch => AdvancedPreferences {
    debug_mode: bool, artificial_delay_ms: u16, forbid_download_copy: bool,
});
patch!(ManagementPatch => ManagementPreferences {
    max_concurrent_transfers: u16, total_rate_limit_mib_per_second: u32,
    clipboard_resource_detection: bool, minecraft_release_notifications: bool,
    minecraft_snapshot_notifications: bool, download_file_name: DownloadFileName,
    mod_display_style: ModDisplayStyle, quick_download: QuickDownload,
});
patch!(UpdatePatch => UpdatePreferences { channel: UpdateChannel, policy: UpdatePolicy });

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherPreferencesPatch {
    pub appearance: Option<AppearancePatch>,
    pub background: Option<BackgroundPatch>,
    pub title: Option<TitlePatch>,
    pub home: Option<HomePatch>,
    pub navigation: Option<NavigationPatch>,
    pub localization: Option<LocalizationPatch>,
    pub announcement_mode: Option<AnnouncementMode>,
    pub animation: Option<AnimationPatch>,
    pub realtime_log_line_limit: Option<u32>,
    pub disable_hardware_acceleration: Option<bool>,
    pub local_diagnostics_enabled: Option<bool>,
    pub network: Option<NetworkPatch>,
    pub advanced: Option<AdvancedPatch>,
    pub updates: Option<UpdatePatch>,
    pub management: Option<ManagementPatch>,
    pub auto_select_installed: Option<bool>,
    pub launch_visibility: Option<LaunchVisibility>,
    pub game_launch: Option<GameLaunchPatch>,
}
impl LauncherPreferencesPatch {
    pub(crate) fn apply(self, value: &mut LauncherPreferences) {
        if let Some(patch) = self.appearance {
            patch.apply(&mut value.appearance);
        }
        if let Some(patch) = self.background {
            patch.apply(&mut value.background);
        }
        if let Some(patch) = self.title {
            patch.apply(&mut value.title);
        }
        if let Some(patch) = self.home {
            patch.apply(&mut value.home);
        }
        if let Some(patch) = self.navigation {
            patch.apply(&mut value.navigation);
        }
        if let Some(patch) = self.localization {
            patch.apply(&mut value.localization);
        }
        if let Some(mode) = self.announcement_mode {
            value.announcement_mode = mode;
        }
        if let Some(patch) = self.animation {
            patch.apply(&mut value.animation);
        }
        if let Some(limit) = self.realtime_log_line_limit {
            value.realtime_log_line_limit = limit;
        }
        if let Some(disabled) = self.disable_hardware_acceleration {
            value.disable_hardware_acceleration = disabled;
        }
        if let Some(enabled) = self.local_diagnostics_enabled {
            value.local_diagnostics_enabled = enabled;
        }
        if let Some(patch) = self.network {
            patch.apply(&mut value.network);
        }
        if let Some(patch) = self.advanced {
            patch.apply(&mut value.advanced);
        }
        if let Some(patch) = self.updates {
            patch.apply(&mut value.updates);
        }
        if let Some(patch) = self.management {
            patch.apply(&mut value.management);
        }
        if let Some(enabled) = self.auto_select_installed {
            value.auto_select_installed = enabled;
        }
        if let Some(visibility) = self.launch_visibility {
            value.launch_visibility = visibility;
        }
        if let Some(patch) = self.game_launch {
            patch.apply(&mut value.game_launch);
        }
    }
}

fn bounded_text(value: &str, label: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(format!("{label}过长或含有控制字符"));
    }
    if value.trim() != value {
        return Err(format!("{label}首尾不能含有空白字符"));
    }
    Ok(())
}

fn font(value: &str) -> Result<(), String> {
    bounded_text(value, "字体名称", 128)?;
    if !value
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err("字体名称只能包含文字、数字、空格、点、横线和下划线".into());
    }
    Ok(())
}

fn local_path(value: &str, label: &str) -> Result<(), String> {
    bounded_text(value, label, 4096)?;
    if value.is_empty() {
        return Ok(());
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
    {
        return Err(format!("{label}需要没有父目录跳转的绝对路径"));
    }
    Ok(())
}

fn url(value: &str, label: &str, proxy: bool) -> Result<(), String> {
    bounded_text(value, label, 2048)?;
    if value.is_empty() {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| format!("{label}不是有效的 URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("{label}只允许带有主机名的 HTTP 或 HTTPS URL"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!("{label}不能包含用户名或密码"));
    }
    if parsed.fragment().is_some() {
        return Err(format!("{label}不能含有片段标识"));
    }
    if proxy && (parsed.path() != "/" || parsed.query().is_some()) {
        return Err("代理地址只允许协议、主机和端口，不支持认证代理".into());
    }
    Ok(())
}

impl LauncherPreferences {
    pub fn validate(&self) -> Result<(), String> {
        self.game_launch.validate()?;
        if !(1..=64).contains(&self.management.max_concurrent_transfers)
            || self.management.total_rate_limit_mib_per_second > 1024
        {
            return Err("下载并发应为 1–64，总速度限制应为 0–1024 MiB/s（0 表示无限）".into());
        }
        if !(20..=100).contains(&self.appearance.opacity_percent) {
            return Err("不透明度应为 20–100%".into());
        }
        for asset_id in [
            &self.background.background_asset_id,
            &self.background.music_asset_id,
        ] {
            if asset_id.len() > 128
                || !asset_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            {
                return Err("背景资源标识无效或过长".into());
            }
        }
        font(&self.appearance.global_font)?;
        font(&self.appearance.motd_font)?;
        bounded_text(&self.title.text, "标题文本", 512)?;
        local_path(&self.title.image_path, "标题图片路径")?;
        local_path(&self.home.local_path, "主页文件路径")?;
        url(&self.home.remote_url, "主页地址", false)?;
        url(&self.network.custom_proxy_url, "代理地址", true)?;
        if self.title.mode == TitleMode::Image && self.title.image_path.is_empty() {
            return Err("图片标题需要选择图片文件".into());
        }
        if self.home.mode == HomeMode::Local && self.home.local_path.is_empty() {
            return Err("本地主页需要选择文件".into());
        }
        if self.home.mode == HomeMode::Remote && self.home.remote_url.is_empty() {
            return Err("联网主页需要填写 HTTP 或 HTTPS 地址".into());
        }
        if self.network.proxy_mode == ProxyMode::Custom && self.network.custom_proxy_url.is_empty()
        {
            return Err("自定义代理需要填写地址".into());
        }
        if self.navigation.hidden_menu_ids.len() > MenuId::ALL.len() {
            return Err("隐藏功能选择超过上限".into());
        }
        let mut ids = BTreeSet::new();
        if self
            .navigation
            .hidden_menu_ids
            .iter()
            .any(|id| !ids.insert(id))
        {
            return Err("隐藏功能选择含有重复标识".into());
        }
        if !(1..=240).contains(&self.animation.fps_limit) {
            return Err("最高动画帧率应为 1–240".into());
        }
        if !(10..=400).contains(&self.animation.speed_percent) {
            return Err("动画速度应为 10–400%".into());
        }
        if !(100..=100_000).contains(&self.realtime_log_line_limit) {
            return Err("实时日志行数应为 100–100000".into());
        }
        if self.advanced.artificial_delay_ms > 5000 {
            return Err("人工延迟应为 0–5000 毫秒".into());
        }
        Ok(())
    }
}
