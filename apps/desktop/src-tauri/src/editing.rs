//! The closed edit action is dispatched to the focused native webview engine.
//! Platform clipboard text never crosses the command boundary.
pub use pcl_desktop_platform::editing::EditAction;

#[tauri::command]
pub async fn launcher_edit(action: EditAction, window: tauri::WebviewWindow) -> Result<(), String> {
    pcl_desktop_platform::editing::edit(window, action).await
}
