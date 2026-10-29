//! CF_UNICODETEXT uses a borrowed global-memory block. Bound the inspected
//! UTF-16 prefix before decoding; never clone an arbitrary clipboard allocation.
use super::*;
use windows_sys::Win32::{
    Foundation::{GlobalFree, HGLOBAL},
    System::{DataExchange::*, Memory::*},
};
const UNICODE_TEXT: u32 = 13;
const ERROR: &str = "系统剪贴板暂时不可用";
struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
struct Locked(HGLOBAL);
impl Drop for Locked {
    fn drop(&mut self) {
        unsafe {
            GlobalUnlock(self.0);
        }
    }
}
pub(super) fn read_text() -> ClipboardRead {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return ClipboardRead::State(ClipboardState::Busy);
        }
        let _open = Open;
        if IsClipboardFormatAvailable(UNICODE_TEXT) == 0 {
            return ClipboardRead::State(ClipboardState::Empty);
        }
        let block = GetClipboardData(UNICODE_TEXT);
        if block.is_null() {
            return ClipboardRead::State(ClipboardState::Unavailable);
        }
        let size = GlobalSize(block);
        if size < 2 || size % 2 != 0 {
            return ClipboardRead::State(ClipboardState::Unsupported);
        }
        let memory = GlobalLock(block).cast::<u16>();
        if memory.is_null() {
            return ClipboardRead::State(ClipboardState::Unavailable);
        }
        let _locked = Locked(block);
        unicode_text(std::slice::from_raw_parts(
            memory,
            (size / 2).min(TEXT_LIMIT + 1),
        ))
    }
}

pub(super) fn write_text(window: &WebviewWindow, text: &str) -> Result<(), String> {
    let owner = window.hwnd().map_err(|_| ERROR)?;
    let units: Vec<_> = text.encode_utf16().chain([0]).collect();
    unsafe {
        // Open with the actual owner. EmptyClipboard after a null owner would
        // prevent SetClipboardData from publishing the new selection.
        if OpenClipboard(owner.0) == 0 {
            return Err(ERROR.into());
        }
        let _open = Open;
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
        if memory.is_null() {
            return Err(ERROR.into());
        }
        let pointer = GlobalLock(memory).cast::<u16>();
        if pointer.is_null() {
            GlobalFree(memory);
            return Err(ERROR.into());
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
        GlobalUnlock(memory);
        if EmptyClipboard() == 0 || SetClipboardData(UNICODE_TEXT, memory).is_null() {
            GlobalFree(memory);
            return Err(ERROR.into());
        }
        // The system owns memory after a successful SetClipboardData.
    }
    Ok(())
}
