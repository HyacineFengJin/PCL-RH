//! Use each engine's native edit command. Clipboard contents never cross IPC,
//! and browser undo history / password-field restrictions remain authoritative.
use serde::Deserialize;
use tauri::WebviewWindow;
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditAction {
    Copy,
    Cut,
    Paste,
    SelectAll,
}
const FAILED: &str = "无法执行文本编辑操作";
impl EditAction {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn command(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Cut => "Cut",
            Self::Paste => "Paste",
            Self::SelectAll => "SelectAll",
        }
    }
}
pub async fn edit(window: WebviewWindow, action: EditAction) -> Result<(), String> {
    if !window.is_focused().map_err(|_| FAILED)? {
        return Err(FAILED.into());
    }
    let (sender, receiver) = tokio::sync::oneshot::channel::<Result<(), String>>();
    window
        .with_webview(move |view| {
            #[cfg(target_os = "linux")]
            {
                use webkit2gtk::WebViewExt;
                view.inner().execute_editing_command(action.command());
                let _ = sender.send(Ok(()));
            }
            #[cfg(target_os = "windows")]
            {
                use std::{cell::RefCell, rc::Rc};
                use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
                use windows::core::{w, HSTRING};
                let sender = Rc::new(RefCell::new(Some(sender)));
                let completion = sender.clone();
                let callback = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                    move |result, _| {
                        if let Some(sender) = completion.borrow_mut().take() {
                            let _ = sender.send(result.map_err(|_| FAILED.to_string()));
                        }
                        Ok(())
                    },
                ));
                // This private adapter accepts four closed edit commands only. It
                // exposes no debugging port, arbitrary protocol method or script.
                let parameters = HSTRING::from(
                    serde_json::json!({"type":"rawKeyDown","commands":[action.command()]})
                        .to_string(),
                );
                let result = unsafe {
                    view.controller().CoreWebView2().and_then(|core| {
                        core.CallDevToolsProtocolMethod(
                            w!("Input.dispatchKeyEvent"),
                            &parameters,
                            &callback,
                        )
                    })
                };
                if result.is_err() {
                    if let Some(sender) = sender.borrow_mut().take() {
                        let _ = sender.send(Err(FAILED.into()));
                    }
                }
            }
            #[cfg(target_os = "macos")]
            {
                use objc2::{sel, MainThreadMarker};
                use objc2_app_kit::NSApplication;
                let result = (|| {
                    let main = MainThreadMarker::new().ok_or(FAILED)?;
                    let application = NSApplication::sharedApplication(main);
                    let selector = match action {
                        EditAction::Copy => sel!(copy:),
                        EditAction::Cut => sel!(cut:),
                        EditAction::Paste => sel!(paste:),
                        EditAction::SelectAll => sel!(selectAll:),
                    };
                    // The responder chain chooses the focused WKWebView editor;
                    // AppKit enforces whether the requested action is available.
                    if unsafe { application.sendAction_to_from(selector, None, None) } {
                        Ok(())
                    } else {
                        Err(FAILED.to_string())
                    }
                })();
                let _ = sender.send(result);
                let _ = view;
            }
        })
        .map_err(|_| FAILED)?;
    tokio::time::timeout(std::time::Duration::from_secs(2), receiver)
        .await
        .map_err(|_| FAILED)?
        .map_err(|_| FAILED)?
}
