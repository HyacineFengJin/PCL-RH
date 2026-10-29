//! The official discovery plugin interprets bounded native text as resource
//! identities. Platform selection ownership lives below the plugin contract.
use crate::product_plugins::contracts::clipboard::{
    ClipboardOutcome, ClipboardState, ModrinthLink,
};
use pcl_desktop_platform::clipboard::{self, ClipboardRead};
const LIMIT: usize = pcl_desktop_platform::clipboard::TEXT_LIMIT;
const REJECTED: &str = "不是受支持的 Modrinth 资源链接";
use ClipboardState as State;
pub fn parse_modrinth_link(text: &str) -> Result<ModrinthLink, String> {
    if text.len() > LIMIT {
        return Err(REJECTED.into());
    }
    let text = text.trim();
    if text
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
        || !text.starts_with("https://modrinth.com/")
    {
        return Err(REJECTED.into());
    }
    let url = reqwest::Url::parse(text).map_err(|_| REJECTED)?;
    if url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != text
    {
        return Err(REJECTED.into());
    }
    let parts = url.path_segments().ok_or(REJECTED)?.collect::<Vec<_>>();
    if parts.len() != 2
        || !matches!(parts[0], "mod" | "resourcepack" | "shader" | "modpack")
        || parts[1].is_empty()
        || parts[1].len() > 128
        || !parts[1]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err(REJECTED.into());
    }
    Ok(ModrinthLink {
        kind: parts[0].into(),
        project_id_or_slug: parts[1].into(),
        url: url.into(),
    })
}

pub fn request_clipboard_link(
    window: &tauri::WebviewWindow,
    enabled: Box<dyn Fn() -> bool>,
    callback: Box<dyn FnOnce(ClipboardOutcome)>,
) -> Result<(), String> {
    clipboard::request_text(
        window,
        enabled,
        Box::new(move |read| {
            let outcome = match read {
                ClipboardRead::State(state) => ClipboardOutcome::state(state),
                ClipboardRead::Text(text) => match parse_modrinth_link(&text) {
                    Ok(link) => ClipboardOutcome {
                        state: State::Found,
                        link: Some(link),
                    },
                    Err(_) => ClipboardOutcome::state(State::Unsupported),
                },
            };
            callback(outcome);
        }),
    )
}
