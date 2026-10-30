//! GDI's fixed Unicode family enumeration; no window or font-file scan.
//! SDK callback/LOGFONT layouts come from the already locked windows-sys.
//! API/callback contract (including return-value and DEFAULT_CHARSET semantics):
//! https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-enumfontfamiliesexw
use super::NativeFamilies;
use windows_sys::Win32::{
    Foundation::LPARAM,
    Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, EnumFontFamiliesExW, DEFAULT_CHARSET, HDC, LOGFONTW,
        TEXTMETRICW,
    },
};

struct DeviceContext(HDC);
impl Drop for DeviceContext {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}
#[derive(Default)]
struct Context {
    families: NativeFamilies,
    error: Option<String>,
}
impl Context {
    fn visit(&mut self, font: *const LOGFONTW) -> Result<(), String> {
        if font.is_null() {
            return Err("系统字体枚举返回空字体描述".into());
        }
        // EnumFontFamiliesExW supplies a valid LOGFONTW during this synchronous
        // callback. Read only the SDK-defined bounded family-name array.
        let units = unsafe { &(*font).lfFaceName };
        let length = units
            .iter()
            .position(|unit| *unit == 0)
            .ok_or("系统字体名称缺少终止边界")?;
        if length == 0 {
            return Err("系统字体名称为空".into());
        }
        let family =
            String::from_utf16(&units[..length]).map_err(|_| "系统字体名称不是有效 Unicode")?;
        self.families.push(&family)
    }
}
unsafe extern "system" fn collect(
    font: *const LOGFONTW,
    _metric: *const TEXTMETRICW,
    _kind: u32,
    parameter: LPARAM,
) -> i32 {
    // GDI calls synchronously on the enumerating worker; Context remains on its
    // stack until EnumFontFamiliesExW returns. No reference escapes the call.
    let context = unsafe { &mut *(parameter as *mut Context) };
    if context.error.is_some() {
        return 0;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.visit(font)))
        .unwrap_or_else(|_| Err("系统字体枚举回调异常".into()));
    match result {
        Ok(()) => 1,
        Err(error) => {
            context.error = Some(error);
            0
        }
    }
}
pub(super) fn enumerate() -> Result<Vec<String>, String> {
    // A memory DC is created and deleted on this worker; there is no window,
    // GetDC(hwnd), selected bitmap/font, drawing, or shared UI-thread handle.
    let dc = DeviceContext(unsafe { CreateCompatibleDC(std::ptr::null_mut()) });
    if dc.0.is_null() {
        return Err("无法创建系统字体枚举设备上下文".into());
    }
    let description = LOGFONTW {
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    let mut context = Context::default();
    let result = unsafe {
        EnumFontFamiliesExW(
            dc.0,
            &description,
            Some(collect),
            (&mut context as *mut Context) as LPARAM,
            0,
        )
    };
    if let Some(error) = context.error {
        return Err(error);
    }
    // The documented result is the last callback value, not GetLastError.
    // Every accepted callback returns one. Zero confirms no successful complete
    // catalogue, so do not return a partial or fabricated success list.
    if result == 0 {
        return Err("系统无法完成字体枚举".into());
    }
    Ok(context.families.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_callback_decodes_unicode_and_stops_on_invalid_utf16() {
        let mut context = Context::default();
        let mut font = LOGFONTW::default();
        let name = "字体 Family".encode_utf16().collect::<Vec<_>>();
        font.lfFaceName[..name.len()].copy_from_slice(&name);
        let parameter = (&mut context as *mut Context) as LPARAM;
        assert_eq!(unsafe { collect(&font, std::ptr::null(), 0, parameter) }, 1);
        font.lfFaceName = [0; 32];
        font.lfFaceName[0] = 0xd800;
        assert_eq!(unsafe { collect(&font, std::ptr::null(), 0, parameter) }, 0);
        assert!(context.error.is_some());
        assert_eq!(context.families.finish(), vec!["字体 Family"]);
    }
}
