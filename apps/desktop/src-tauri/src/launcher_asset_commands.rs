//! Native pickers authorize local sources; media is served by opaque catalog IDs
//! through a custom protocol. Preference import alone never reads a homepage or
//! authorizes an external title-image pathname.
use crate::{launcher_assets as assets, launcher_prefs::*, Shared};
use serde::Serialize;
use std::{path::Path, sync::Arc, time::Duration};
use tauri::{Manager, State};

#[derive(Serialize)]
pub struct TitleChoice {
    view: LauncherPreferencesView,
    asset: assets::AssetRow,
}
#[derive(Serialize)]
pub struct HomeChoice {
    view: LauncherPreferencesView,
    home: assets::HomepageRead,
}

#[tauri::command]
pub async fn launcher_assets(
    collection: assets::AssetCollection,
    state: State<'_, Arc<Shared>>,
) -> Result<Vec<assets::AssetRow>, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || shared.launcher_assets.list(collection))
        .await
        .map_err(|_| "读取媒体目录的任务意外退出".to_string())?
}
#[tauri::command]
pub async fn launcher_open_media_folder(
    collection: assets::AssetCollection,
    state: State<'_, Arc<Shared>>,
) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = shared.launcher_assets.ensure_folder(collection)?;
        pcl_desktop_platform::opener::open_path(&path)
            .map_err(|error| format!("无法打开媒体目录：{error}"))
    })
    .await
    .map_err(|_| "打开媒体目录的任务意外退出".to_string())?
}
#[tauri::command]
pub async fn launcher_fonts() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(assets::discover_font_families)
        .await
        .map_err(|_| "读取系统字体的任务意外退出".to_string())?
}
#[tauri::command]
pub async fn launcher_title_asset(
    state: State<'_, Arc<Shared>>,
) -> Result<Option<assets::AssetRow>, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prefs = shared.launcher_preferences.snapshot().preferences;
        if prefs.title.mode != TitleMode::Image {
            return Ok(None);
        }
        shared
            .launcher_assets
            .read_title(Path::new(&prefs.title.image_path))
            .map(Some)
    })
    .await
    .map_err(|_| "读取标题图片的任务意外退出".to_string())?
}
#[tauri::command]
pub async fn launcher_title_pick(
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<TitleChoice>, String> {
    let shared = state.inner().clone();
    shared.launcher_preferences.export_settings(&revision)?;
    let choice = shared
        .desktop
        .choose_launcher_file(window.clone(), shared.project.clone(), "title_image")
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    let source = choice
        .paths
        .first()
        .cloned()
        .ok_or_else(|| choice.message.unwrap_or_else(|| "未选择有效图片".into()))?;
    let mut result = tauri::async_runtime::spawn_blocking(move || {
        let _admission = shared.operations.lock().unwrap();
        if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        shared.launcher_preferences.export_settings(&revision)?;
        let plan = assets::prepare_title_import(&source)?;
        let asset = shared
            .launcher_assets
            .import_title(&source, &plan.revision)?;
        let view = shared.launcher_preferences.update(
            &revision,
            LauncherPreferencesPatch {
                title: Some(TitlePatch {
                    mode: Some(TitleMode::Image),
                    image_path: Some(asset.path.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        Ok::<_, String>(TitleChoice { view, asset })
    })
    .await
    .map_err(|_| "导入标题图片的任务意外退出".to_string())??;
    crate::launcher_commands::finish_window_effect(&state, &window, &mut result.view);
    Ok(Some(result))
}
#[tauri::command]
pub async fn launcher_home_pick(
    revision: String,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<Shared>>,
) -> Result<Option<HomeChoice>, String> {
    let shared = state.inner().clone();
    shared.launcher_preferences.export_settings(&revision)?;
    let choice = shared
        .desktop
        .choose_launcher_file(window, shared.project.clone(), "home_file")
        .await?;
    if choice.status == "cancelled" {
        return Ok(None);
    }
    let source = choice.paths.first().cloned().ok_or_else(|| {
        choice
            .message
            .unwrap_or_else(|| "未选择有效的主页文件".into())
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        let _admission = shared.operations.lock().unwrap();
        if shared.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("启动器正在关闭".into());
        }
        let home = assets::read_homepage(&source)?;
        let view = shared.launcher_preferences.update(
            &revision,
            LauncherPreferencesPatch {
                home: Some(HomePatch {
                    mode: Some(HomeMode::Local),
                    local_path: Some(source.display().to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        Ok(Some(HomeChoice { view, home }))
    })
    .await
    .map_err(|_| "选择主页的任务意外退出".to_string())?
}

async fn fetch_home(url: &str) -> Result<assets::HomepageRead, String> {
    let factory = pcl_network::snapshot();
    let status = factory.status();
    if matches!(status.policy.proxy, pcl_network::ProxyPolicy::Custom(_))
        || status.system_proxy_configured
    {
        return Err(
            "联网主页需要直连以固定经过校验的目标地址；请将 HTTP 代理设为不使用后重试".into(),
        );
    }
    let mut next = url.to_owned();
    for _ in 0..=4 {
        let location = assets::validate_homepage_url(&next)?;
        let addresses = factory.resolve_host(&location.host).await?;
        assets::validate_public_addresses(&addresses)?;
        let sockets: Vec<_> = addresses
            .into_iter()
            .map(|ip| std::net::SocketAddr::new(ip, location.port))
            .collect();
        let client = factory
            .async_client()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&location.host, &sockets)
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(15))
            .user_agent("PCL-RH/0.2.0")
            .build()
            .map_err(|_| "无法建立主页连接")?;
        let mut response = client
            .get(&location.url)
            .send()
            .await
            .map_err(|_| "主页连接失败")?;
        if response.status().is_redirection() {
            let redirect = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("主页重定向缺少有效地址")?;
            next = reqwest::Url::parse(&location.url)
                .map_err(|_| "主页地址无效")?
                .join(redirect)
                .map_err(|_| "主页重定向地址无效")?
                .to_string();
            continue;
        }
        response
            .error_for_status_ref()
            .map_err(|_| "主页服务返回错误")?;
        if response
            .content_length()
            .is_some_and(|len| len > 256 * 1024)
        {
            return Err("主页响应超过 256 KiB".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "主页读取失败")? {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err("主页响应超过 256 KiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let page = assets::parse_homepage(&bytes)?;
        use sha2::{Digest, Sha256};
        return Ok(assets::HomepageRead {
            revision: format!("{:x}", Sha256::digest(&bytes)),
            page,
        });
    }
    Err("主页重定向过多".into())
}
#[tauri::command]
pub async fn launcher_home_refresh(
    state: State<'_, Arc<Shared>>,
) -> Result<Option<assets::HomepageRead>, String> {
    let shared = state.inner().clone();
    let prefs = shared.launcher_preferences.snapshot().preferences;
    match prefs.home.mode {
        HomeMode::Blank | HomeMode::Preset => Ok(None),
        HomeMode::Local => tauri::async_runtime::spawn_blocking(move || {
            assets::read_homepage(Path::new(&prefs.home.local_path)).map(Some)
        })
        .await
        .map_err(|_| "读取主页的任务意外退出".to_string())?,
        HomeMode::Remote => {
            tokio::time::timeout(Duration::from_secs(30), fetch_home(&prefs.home.remote_url))
                .await
                .map_err(|_| "主页读取超时".to_string())?
                .map(Some)
        }
    }
}
#[tauri::command]
pub fn launcher_home_open_link(url: String) -> Result<(), String> {
    let link = assets::validate_homepage_link(&url)?;
    pcl_desktop_platform::opener::open_url(&link)
}

pub fn serve_media(
    context: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let shared = context.app_handle().state::<Arc<Shared>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::http::StatusCode;
        let response = (|| {
            if !matches!(request.method().as_str(), "GET" | "HEAD") {
                return Err(StatusCode::METHOD_NOT_ALLOWED);
            }
            let mut parts = request.uri().path().trim_start_matches('/').split('/');
            let collection = match parts.next() {
                Some("backgrounds") => assets::AssetCollection::Backgrounds,
                Some("music") => assets::AssetCollection::Music,
                Some("titles") => assets::AssetCollection::Titles,
                _ => return Err(StatusCode::NOT_FOUND),
            };
            let id = parts.next().ok_or(StatusCode::NOT_FOUND)?;
            if parts.next().is_some() || request.uri().query().is_some() {
                return Err(StatusCode::NOT_FOUND);
            }
            let range = request
                .headers()
                .get("Range")
                .map(|v| v.to_str().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE))
                .transpose()?;
            let asset = shared
                .launcher_assets
                .read_asset(collection, id, range)
                .map_err(|_| {
                    if range.is_some() {
                        StatusCode::RANGE_NOT_SATISFIABLE
                    } else {
                        StatusCode::NOT_FOUND
                    }
                })?;
            let mut response = tauri::http::Response::builder()
                .status(if asset.partial { 206 } else { 200 })
                .header("Content-Type", asset.mime)
                .header("Content-Length", asset.bytes.len())
                .header("Accept-Ranges", "bytes")
                .header("Access-Control-Allow-Origin", "*")
                .header("ETag", format!("\"{id}\""))
                .header("X-Content-Type-Options", "nosniff")
                .header("Cache-Control", "no-store");
            if asset.partial {
                response = response.header(
                    "Content-Range",
                    format!("bytes {}-{}/{}", asset.start, asset.end, asset.total),
                );
            }
            response
                .body(if request.method().as_str() == "HEAD" {
                    Vec::new()
                } else {
                    asset.bytes
                })
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        })();
        responder.respond(response.unwrap_or_else(|status| {
            tauri::http::Response::builder()
                .status(status)
                .body(Vec::new())
                .unwrap()
        }));
    });
}
