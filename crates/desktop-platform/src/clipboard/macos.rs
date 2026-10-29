use super::*;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

pub(super) fn read_text() -> ClipboardRead {
    objc2::rc::autoreleasepool(|_| {
        let clipboard = NSPasteboard::generalPasteboard();
        let Some(text) = clipboard.stringForType(unsafe { NSPasteboardTypeString }) else {
            return ClipboardRead::State(ClipboardState::Empty);
        };
        // NSString reports UTF-16 units. Check before creating a Rust string,
        // then enforce the shared UTF-8 byte limit as well.
        if text.length() > TEXT_LIMIT {
            return ClipboardRead::State(ClipboardState::Unsupported);
        }
        bounded_text(text.to_string())
    })
}
pub(super) fn write_text(_window: &WebviewWindow, text: &str) -> Result<(), String> {
    objc2::rc::autoreleasepool(|_| {
        let clipboard = NSPasteboard::generalPasteboard();
        let value = NSString::from_str(text);
        clipboard.clearContents();
        if clipboard.setString_forType(&value, unsafe { NSPasteboardTypeString }) {
            Ok(())
        } else {
            Err("系统剪贴板暂时不可用".into())
        }
    })
}
