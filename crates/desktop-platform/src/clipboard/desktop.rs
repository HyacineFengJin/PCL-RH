//! Windows/macOS clipboard owners can answer synchronously and slowly. Use one
//! worker for the admitted read; a timer retires only its delivery callback.
//! All permission/focus checks and callback ownership stay on the event thread.
use super::{
    flight::{Dispatch, Pending},
    *,
};
use std::{
    cell::{Cell, RefCell},
    time::Duration,
};

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
    static NEXT: Cell<u64> = const { Cell::new(0) };
}
fn deliver(dispatch: Option<Dispatch>) {
    if let Some((callback, read)) = dispatch {
        callback(read);
    }
}
fn finish(id: u64, read: ClipboardRead) {
    let request = PENDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|request| request.id == id) {
            slot.take()
        } else {
            None
        }
    });
    deliver(request.and_then(|request| request.finish(read)));
}
fn timeout(id: u64) {
    let dispatch = PENDING.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .filter(|request| request.id == id)
            .and_then(Pending::timeout)
    });
    deliver(dispatch);
}
pub(super) fn request_text(
    window: &WebviewWindow,
    permitted: ReadPermission,
    complete: ReadCallback,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let on_event_thread = objc2::MainThreadMarker::new().is_some();
    #[cfg(target_os = "windows")]
    let on_event_thread = {
        use windows_sys::Win32::{
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::GetWindowThreadProcessId,
        };
        let owner = window.hwnd().map_err(|_| "无法访问系统剪贴板窗口")?;
        unsafe { GetWindowThreadProcessId(owner.0, std::ptr::null_mut()) == GetCurrentThreadId() }
    };
    if !on_event_thread {
        return Err("剪贴板请求必须在桌面主线程执行".into());
    }
    if !permitted() {
        complete(ClipboardRead::State(ClipboardState::Disabled));
        return Ok(());
    }
    if !window.is_focused().unwrap_or(false) {
        complete(ClipboardRead::State(ClipboardState::NotFocused));
        return Ok(());
    }
    if PENDING.with(|slot| slot.borrow().is_some()) {
        complete(ClipboardRead::State(ClipboardState::Busy));
        return Ok(());
    }
    let id = NEXT
        .with(|next| {
            let id = next.get().checked_add(1)?;
            next.set(id);
            Some(id)
        })
        .ok_or("剪贴板请求序号已耗尽")?;
    let focused = window.clone();
    PENDING.with(|slot| {
        *slot.borrow_mut() = Some(Pending {
            id,
            permitted,
            focused: Box::new(move || focused.is_focused().unwrap_or(false)),
            complete: Some(complete),
        })
    });
    let reader_window = window.clone();
    let spawned = std::thread::Builder::new()
        .name("pcl-clipboard-read".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(|| {
                #[cfg(target_os = "windows")]
                return super::windows::read_text();
                #[cfg(target_os = "macos")]
                return super::macos::read_text();
            })
            .unwrap_or(ClipboardRead::State(ClipboardState::Unavailable));
            let _ = reader_window.run_on_main_thread(move || finish(id, result));
        });
    if spawned.is_err() {
        finish(id, ClipboardRead::State(ClipboardState::Unavailable));
        return Ok(());
    }
    let timer_window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = timer_window.run_on_main_thread(move || timeout(id));
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    #[test]
    fn late_worker_and_timer_cannot_retire_a_different_read() {
        let count = Rc::new(Cell::new(0));
        for id in [7, 8] {
            let called = count.clone();
            PENDING.with(|slot| {
                *slot.borrow_mut() = Some(Pending {
                    id,
                    permitted: Box::new(|| true),
                    focused: Box::new(|| true),
                    complete: Some(Box::new(move |_| called.set(called.get() + 1))),
                })
            });
            timeout(id - 1);
            finish(id - 1, ClipboardRead::Text("stale private text".into()));
            assert!(PENDING.with(|slot| slot
                .borrow()
                .as_ref()
                .is_some_and(|request| request.id == id && request.complete.is_some())));
            finish(id, ClipboardRead::State(ClipboardState::Empty));
            assert!(PENDING.with(|slot| slot.borrow().is_none()));
        }
        assert_eq!(count.get(), 2);
    }
}
