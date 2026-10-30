//! CoreText family names with bounded, lossless CoreFoundation UTF-8 conversion.
//! Opaque references stay on the admitted worker and the Copy result is released.
//! Apple documents individual CoreText calls as thread-safe:
//! https://developer.apple.com/documentation/CoreText
//! CFStringGetBytes uses lossByte=0 and must convert the complete UTF-16 range:
//! https://developer.apple.com/documentation/corefoundation/cfstringgetbytes
use super::{NativeFamilies, MAX_OUTPUT};
use std::ffi::c_void;

type Reference = *const c_void;
#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}
#[link(name = "CoreText", kind = "framework")]
extern "C" {
    fn CTFontManagerCopyAvailableFontFamilyNames() -> Reference;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(reference: Reference);
    fn CFGetTypeID(reference: Reference) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: Reference) -> isize;
    fn CFArrayGetValueAtIndex(array: Reference, index: isize) -> Reference;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(string: Reference) -> isize;
    fn CFStringGetBytes(
        string: Reference,
        range: Range,
        encoding: u32,
        loss_byte: u8,
        external_representation: u8,
        buffer: *mut u8,
        capacity: isize,
        used: *mut isize,
    ) -> isize;
}
struct Array(Reference);
impl Drop for Array {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}
pub(super) fn enumerate() -> Result<Vec<String>, String> {
    let reference = unsafe { CTFontManagerCopyAvailableFontFamilyNames() };
    if reference.is_null() {
        return Err("CoreText 无法获取系统字体列表".into());
    }
    let array = Array(reference);
    if unsafe { CFGetTypeID(array.0) != CFArrayGetTypeID() } {
        return Err("CoreText 返回了无效字体列表".into());
    }
    let count = unsafe { CFArrayGetCount(array.0) };
    if count < 0 || count as usize > MAX_OUTPUT {
        return Err("CoreText 字体列表超过枚举边界".into());
    }
    let mut catalog = NativeFamilies::default();
    for index in 0..count {
        let string = unsafe { CFArrayGetValueAtIndex(array.0, index) };
        if string.is_null() || unsafe { CFGetTypeID(string) != CFStringGetTypeID() } {
            return Err("CoreText 返回了无效字体名称对象".into());
        }
        let length = unsafe { CFStringGetLength(string) };
        // At most 128 Unicode scalars, represented by at most 256 UTF-16 units
        // and 512 UTF-8 bytes. No font-supplied allocation size is trusted.
        if !(1..=256).contains(&length) {
            return Err("CoreText 字体名称长度无效".into());
        }
        let mut buffer = [0u8; 512];
        let mut used = 0isize;
        let converted = unsafe {
            CFStringGetBytes(
                string,
                Range {
                    location: 0,
                    length,
                },
                0x0800_0100,
                0,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                &mut used,
            )
        };
        if converted != length || used < 0 || used as usize > buffer.len() {
            return Err("CoreText 字体名称无法无损转换为 UTF-8".into());
        }
        let family = std::str::from_utf8(&buffer[..used as usize])
            .map_err(|_| "CoreText 字体名称不是有效 UTF-8")?;
        catalog.push(family)?;
    }
    Ok(catalog.finish())
}
