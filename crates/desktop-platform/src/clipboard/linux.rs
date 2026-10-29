//! Linux selection ownership stays on GTK's thread. Its asynchronous request
//! cannot be cancelled; retain single-flight admission after a timeout until the
//! native callback arrives. Never run a synchronous clipboard read on this thread.
use super::*;
use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use ClipboardState as State;
thread_local! {static PENDING:Cell<bool>=const {Cell::new(false)};}
/// Call inside Tauri run_on_main_thread with its actual GTK window and a live
/// preference reader. Both permission and native keyboard focus are rechecked
/// before the request and before returning an approved link to the application.
pub(super) fn request_text(
    window: &WebviewWindow,
    enabled: ReadPermission,
    callback: ReadCallback,
) -> Result<(), String> {
    if !gtk::is_initialized_main_thread() {
        return Err("剪贴板请求必须在已初始化的 GTK 主线程执行".into());
    }
    if !enabled() {
        callback(ClipboardRead::State(State::Disabled));
        return Ok(());
    }
    let window = window
        .gtk_window()
        .map_err(|_| "无法访问系统剪贴板窗口")?
        .upcast::<gtk::Window>();
    if !window.is_active() || !window.has_toplevel_focus() {
        callback(ClipboardRead::State(State::NotFocused));
        return Ok(());
    }
    if PENDING.with(|pending| pending.get()) {
        callback(ClipboardRead::State(State::Busy));
        return Ok(());
    }
    let Some(display) = gdk::Display::default() else {
        callback(ClipboardRead::State(State::Unavailable));
        return Ok(());
    };
    let Some(clipboard) = gtk::Clipboard::default(&display) else {
        callback(ClipboardRead::State(State::Unavailable));
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
                callback(ClipboardRead::State(State::TimedOut));
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
            ClipboardRead::State(State::Disabled)
        } else if !window.is_active() || !window.has_toplevel_focus() {
            ClipboardRead::State(State::NotFocused)
        } else {
            match text {
                None | Some("") => ClipboardRead::State(State::Empty),
                Some(text) if text.len() <= TEXT_LIMIT => bounded_text(text.to_owned()),
                Some(_) => ClipboardRead::State(State::Unsupported),
            }
        };
        callback(outcome);
    });
    Ok(())
}

pub(super) fn write_text(_window: &WebviewWindow, text: &str) -> Result<(), String> {
    if !gtk::is_initialized_main_thread() {
        return Err("剪贴板请求必须在桌面主线程执行".into());
    }
    let display = gdk::Display::default().ok_or("系统剪贴板不可用")?;
    let clipboard = gtk::Clipboard::default(&display).ok_or("系统剪贴板不可用")?;
    clipboard.set_text(text);
    Ok(())
}
