use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
pub struct Desktop {
    choosing: AtomicBool,
}

#[derive(Serialize)]
pub struct DirectoryChoice {
    status: &'static str,
    path: Option<String>,
    message: Option<String>,
}

pub struct ResourceChoice {
    pub status: &'static str,
    pub paths: Vec<PathBuf>,
    pub message: Option<String>,
}

impl ResourceChoice {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: "unavailable",
            paths: Vec::new(),
            message: Some(message.into()),
        }
    }
}

impl DirectoryChoice {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: "unavailable",
            path: None,
            message: Some(message.into()),
        }
    }
}

struct ChoosingGuard(Arc<Desktop>);
impl Drop for ChoosingGuard {
    fn drop(&mut self) {
        self.0.choosing.store(false, Ordering::SeqCst);
    }
}

impl Desktop {
    pub async fn choose_launcher_file(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
        kind: &str,
    ) -> Result<ResourceChoice, String> {
        self.choose_named_launcher_file(window, initial, kind, None)
            .await
    }

    /// A fixed operation selects the dialog contract. The selected pathname is
    /// only a proposal: services still validate its type, ownership and revision
    /// after the user finishes the chooser.
    pub async fn choose_named_launcher_file(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
        kind: &str,
        suggested: Option<String>,
    ) -> Result<ResourceChoice, String> {
        let (title, save, extension, extensions, name) = match kind {
            "export_settings" => (
                "导出启动器设置",
                true,
                "json",
                vec!["json"],
                "launcher-settings.json",
            ),
            "extension_manifest" => (
                "选择插件声明或兼容包",
                false,
                "json",
                vec!["json", "pclext", "pnp", "pclx"],
                "",
            ),
            "import_settings" => ("导入启动器设置", false, "json", vec!["json"], ""),
            "export_log_zip" => (
                "导出全部游戏日志",
                true,
                "zip",
                vec!["zip"],
                "game-logs.zip",
            ),
            "export_log_text" => ("导出游戏日志", true, "log", vec!["log", "txt"], "game.log"),
            "export_instance_log_text" => (
                "导出当前日志文本",
                true,
                "txt",
                vec!["txt"],
                "instance-log.excerpt.txt",
            ),
            "title_image" => (
                "选择标题图片",
                false,
                "png",
                vec!["png", "jpg", "jpeg", "gif", "webp"],
                "",
            ),
            "home_file" => ("选择主页文件", false, "json", vec!["json"], ""),
            "skin_file" => ("选择本地皮肤", false, "png", vec!["png"], ""),
            "export_tool_image" => ("保存图片", true, "png", vec!["png"], "image.png"),
            "export_resource_info" => (
                "导出资源信息",
                true,
                "json",
                vec!["json"],
                "resource-info.json",
            ),
            _ => return Err("未知启动器文件操作".into()),
        };
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let mut picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title(title)
            .add_filter("支持的文件", &extensions);
        if initial.is_dir() {
            picker = picker.set_directory(initial);
        }
        if save {
            picker = picker.set_file_name(suggested.as_deref().unwrap_or(name));
        }
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            let picked = if save {
                picker.blocking_save_file()
            } else {
                picker.blocking_pick_file()
            };
            match picked {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(mut path) if path.is_absolute() && path.to_str().is_some() => {
                        if save && path.extension().is_none() {
                            path.set_extension(extension);
                        }
                        if !extensions.iter().any(|ext| {
                            path.extension()
                                .and_then(|v| v.to_str())
                                .is_some_and(|value| value.eq_ignore_ascii_case(ext))
                        }) {
                            return ResourceChoice::unavailable("请选择支持的文件格式");
                        }
                        ResourceChoice {
                            status: "selected",
                            paths: vec![path],
                            message: None,
                        }
                    }
                    _ => ResourceChoice::unavailable("所选位置不是可访问的本地文件路径"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面文件选择服务未能完成请求")))
    }

    pub async fn pick_instance_zip(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
    ) -> Result<ResourceChoice, String> {
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("导入本地整合包")
            .add_filter("本地整合包 ZIP / Modrinth", &["zip", "mrpack"]);
        let picker = if initial.is_dir() {
            picker.set_directory(initial)
        } else {
            picker
        };
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_pick_file() {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(path) if path.is_absolute() && path.to_str().is_some() => ResourceChoice {
                        status: "selected",
                        paths: vec![path],
                        message: None,
                    },
                    _ => ResourceChoice::unavailable("请选择可访问的本地 ZIP 或 mrpack 文件"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面文件选择服务未能完成请求")))
    }

    pub async fn pick_java(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
    ) -> Result<ResourceChoice, String> {
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("选择 Java 可执行文件");
        let picker = if initial.is_dir() {
            picker.set_directory(initial)
        } else {
            picker
        };
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_pick_file() {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(path) if path.is_absolute() && path.is_file() && path.to_str().is_some() => {
                        ResourceChoice {
                            status: "selected",
                            paths: vec![path],
                            message: None,
                        }
                    }
                    _ => ResourceChoice::unavailable("请选择可访问的本地 Java 可执行文件"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面文件选择服务未能完成请求")))
    }

    pub async fn save_zip(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
        suggested: String,
    ) -> Result<ResourceChoice, String> {
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("导出实例")
            .add_filter("ZIP 压缩文件", &["zip"])
            .set_file_name(suggested);
        let picker = if initial.is_dir() {
            picker.set_directory(initial)
        } else {
            picker
        };
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_save_file() {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(mut path) if path.is_absolute() && path.to_str().is_some() => {
                        if path.extension().is_none() {
                            path.set_extension("zip");
                        }
                        if path
                            .extension()
                            .and_then(|s| s.to_str())
                            .is_none_or(|s| !s.eq_ignore_ascii_case("zip"))
                        {
                            return ResourceChoice::unavailable("请使用 .zip 文件名保存导出结果");
                        }
                        ResourceChoice {
                            status: "selected",
                            paths: vec![path],
                            message: None,
                        }
                    }
                    _ => ResourceChoice::unavailable("所选位置不是可访问的本地文件路径"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面保存窗口未能完成请求")))
    }

    pub async fn pick_resource_files(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
        kind: &str,
    ) -> Result<ResourceChoice, String> {
        let (title, extensions) = match kind {
            "mods" => ("选择模组文件", vec!["jar", "disabled"]),
            "resourcepacks" => ("选择资源包文件", vec!["zip"]),
            "shaderpacks" => ("选择光影包文件", vec!["zip"]),
            _ => return Err("此资源类型暂不支持从文件安装".into()),
        };
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title(title)
            .add_filter("Minecraft 资源", &extensions);
        let picker = if initial.is_dir() {
            picker.set_directory(initial)
        } else {
            picker
        };
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_pick_files() {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(files) => {
                    let mut paths = Vec::new();
                    for file in files {
                        match file.into_path() {
                            Ok(path)
                                if path.is_absolute()
                                    && path.is_file()
                                    && path.to_str().is_some() =>
                            {
                                paths.push(path);
                            }
                            _ => {
                                return ResourceChoice::unavailable(
                                    "所选位置包含不可访问的本地文件或无效路径",
                                )
                            }
                        }
                    }
                    ResourceChoice {
                        status: if paths.is_empty() {
                            "cancelled"
                        } else {
                            "selected"
                        },
                        paths,
                        message: None,
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面文件选择服务未能完成请求")))
    }

    pub async fn pick_root(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
    ) -> Result<DirectoryChoice, String> {
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(DirectoryChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("选择游戏文件夹");
        let picker = if initial.is_dir() {
            picker.set_directory(initial)
        } else {
            picker
        };
        let choice = tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_pick_folder() {
                None => DirectoryChoice {
                    status: "cancelled",
                    path: None,
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(path) if path.is_absolute() && path.is_dir() => match path.to_str() {
                        Some(path) => DirectoryChoice {
                            status: "selected",
                            path: Some(path.to_owned()),
                            message: None,
                        },
                        None => DirectoryChoice::unavailable("所选文件夹路径不是有效的 UTF-8"),
                    },
                    _ => DirectoryChoice::unavailable("所选位置不是可访问的本地文件夹"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| DirectoryChoice::unavailable("桌面文件夹选择服务未能完成请求"));
        Ok(choice)
    }
}

impl Desktop {
    /// A selected download folder is independent of registered game roots.
    /// The owning service captures its descriptor and validates its identity.
    pub async fn choose_toolbox_directory(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
    ) -> Result<ResourceChoice, String> {
        self.choose_named_toolbox_directory(window, initial, "选择下载保存文件夹")
            .await
    }

    pub async fn choose_named_toolbox_directory(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
        title: &str,
    ) -> Result<ResourceChoice, String> {
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let mut picker = window.dialog().file().set_parent(&window).set_title(title);
        if initial.is_dir() {
            picker = picker.set_directory(initial);
        }
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_pick_folder() {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(path) if path.is_absolute() && path.to_str().is_some() => ResourceChoice {
                        status: "selected",
                        paths: vec![path],
                        message: None,
                    },
                    _ => ResourceChoice::unavailable("请选择可访问的本地文件夹"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面文件夹选择服务未能完成请求")))
    }

    /// Resource saves use their authoritative suggested filename. A picker
    /// result grants a proposed destination, not permission to overwrite it.
    pub async fn choose_resource_save(
        self: &Arc<Self>,
        window: tauri::WebviewWindow,
        initial: PathBuf,
        suggested: String,
    ) -> Result<ResourceChoice, String> {
        if self.choosing.swap(true, Ordering::SeqCst) {
            return Err("已有文件选择窗口，请先完成或取消选择".into());
        }
        let guard = ChoosingGuard(self.clone());
        #[cfg(target_os = "linux")]
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Ok(ResourceChoice::unavailable(
                "无法连接桌面文件选择服务，请在桌面会话中运行启动器",
            ));
        }
        let mut picker = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("保存资源文件")
            .set_file_name(&suggested);
        if initial.is_dir() {
            picker = picker.set_directory(initial);
        }
        Ok(tauri::async_runtime::spawn_blocking(move || {
            let _guard = guard;
            match picker.blocking_save_file() {
                None => ResourceChoice {
                    status: "cancelled",
                    paths: Vec::new(),
                    message: None,
                },
                Some(file) => match file.into_path() {
                    Ok(path) if path.is_absolute() && path.to_str().is_some() => ResourceChoice {
                        status: "selected",
                        paths: vec![path],
                        message: None,
                    },
                    _ => ResourceChoice::unavailable("请选择可访问的本地文件路径"),
                },
            }
        })
        .await
        .unwrap_or_else(|_| ResourceChoice::unavailable("桌面文件选择服务未能完成请求")))
    }
}
