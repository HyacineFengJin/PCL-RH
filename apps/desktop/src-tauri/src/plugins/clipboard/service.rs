//! Focused, opt-in GTK clipboard discovery owned by the official plugin.
//! Raw text stays native. GTK cannot pre-limit the compositor transfer; the
//! post-read 4 KiB cap prevents cloning/parsing oversized selections. No shell
//! fallback, persistent clipboard history or source-text log is created.
use crate::product_plugins::contracts::clipboard::{
    ClipboardOutcome, ClipboardState, ModrinthLink,
};
use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
const LIMIT: usize = 4096;
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
thread_local! {static PENDING:Cell<bool>=const {Cell::new(false)};}
/// Call inside Tauri run_on_main_thread with its actual GTK window and a live
/// preference reader. Both permission and native keyboard focus are rechecked
/// before the request and before returning an approved link to the application.
pub fn request_clipboard_link(
    window: &impl IsA<gtk::Window>,
    enabled: impl Fn() -> bool + 'static,
    callback: impl FnOnce(ClipboardOutcome) + 'static,
) -> Result<(), String> {
    if !gtk::is_initialized_main_thread() {
        return Err("剪贴板请求必须在已初始化的 GTK 主线程执行".into());
    }
    if !enabled() {
        callback(ClipboardOutcome::state(State::Disabled));
        return Ok(());
    }
    let window = window.as_ref().clone();
    if !window.is_active() || !window.has_toplevel_focus() {
        callback(ClipboardOutcome::state(State::NotFocused));
        return Ok(());
    }
    if PENDING.with(|pending| pending.get()) {
        callback(ClipboardOutcome::state(State::Busy));
        return Ok(());
    }
    let Some(display) = gdk::Display::default() else {
        callback(ClipboardOutcome::state(State::Unavailable));
        return Ok(());
    };
    let Some(clipboard) = gtk::Clipboard::default(&display) else {
        callback(ClipboardOutcome::state(State::Unavailable));
        return Ok(());
    };
    PENDING.with(|pending| pending.set(true));
    let complete = Rc::new(RefCell::new(Some(callback)));
    let timeout_id = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let timed_complete = complete.clone();
    let timed_id = timeout_id.clone();
    *timeout_id.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
        Duration::from_secs(2),
        move || {
            timed_id.borrow_mut().take();
            let callback = timed_complete.borrow_mut().take();
            if let Some(callback) = callback {
                callback(ClipboardOutcome::state(State::TimedOut));
            }
            // request_text has no cancellation API. Keep the native single-flight
            // permit until its callback arrives, avoiding unbounded pending requests.
        },
    ));
    clipboard.request_text(move |_, text| {
        PENDING.with(|pending| pending.set(false));
        if let Some(timeout) = timeout_id.borrow_mut().take() {
            timeout.remove();
        }
        let Some(callback) = complete.borrow_mut().take() else {
            return;
        };
        let outcome = if !enabled() {
            ClipboardOutcome::state(State::Disabled)
        } else if !window.is_active() || !window.has_toplevel_focus() {
            ClipboardOutcome::state(State::NotFocused)
        } else {
            match text {
                None | Some("") => ClipboardOutcome::state(State::Empty),
                Some(text) if text.len() <= LIMIT => match parse_modrinth_link(text) {
                    Ok(link) => ClipboardOutcome {
                        state: State::Found,
                        link: Some(link),
                    },
                    Err(_) => ClipboardOutcome::state(State::Unsupported),
                },
                Some(_) => ClipboardOutcome::state(State::Unsupported),
            }
        };
        callback(outcome);
    });
    Ok(())
}
