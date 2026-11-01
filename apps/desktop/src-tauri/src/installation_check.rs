//! Explicit installer QA through the actual layout and native plugin host.
//! Build identity remains a separate read-only command. This opt-in command
//! creates ordinary state only under the selected data root and never starts
//! an account, a game, AI or a WebView.
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) fn dispatch_cli() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("--installation-check")) {
        return None;
    }
    if args.next().is_some() {
        eprintln!("--installation-check accepts no additional arguments");
        return Some(2);
    }
    let result = run();
    let (code, value) = match result {
        Ok(value) => (0, value),
        Err(error) => (1, json!({"schema_version":1,"ok":false,"error":error})),
    };
    let mut output = std::io::stdout().lock();
    if serde_json::to_writer(&mut output, &value).is_err()
        || output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .is_err()
    {
        return Some(1);
    }
    Some(code)
}

fn run() -> Result<Value, String> {
    let layout = crate::launcher_paths::resolve()?;
    if !layout.installed {
        return Err("安装检查需要独立 .app 或显式的 PCL_RH_RESOURCE_ROOT".into());
    }
    layout.initialize()?;
    let (config, warning) = crate::config::ConfigStore::load(&layout.project);
    if let Some(warning) = warning {
        return Err(warning);
    }
    let (settings, _) = config.view();
    let host = Arc::new(crate::experimental::Host::with_resources(
        layout.project.clone(),
        layout.resources.clone(),
    ));
    let checked = host.installation_check();
    host.shutdown();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !host.is_stopped() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !host.is_stopped() {
        return Err("安装检查的插件宿主尚未完成退役".into());
    }
    let plugin = checked?;
    if !plugin["entries"].is_array() {
        return Err("插件宿主返回了无效的列表".into());
    }
    Ok(json!({"schema_version":1,"ok":true,"installed":true,
        "resource_root":layout.resources,"data_root":layout.project,
        "game_root":settings.root,"plugin_host_checked":true,"plugin_host_retired":true,
        "desktop_or_game_started":false}))
}
