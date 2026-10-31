//! Real platform sources; Linux's old anonymous protocol remains unchanged.
#[cfg(target_os = "linux")]
#[path = "files/linux.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
#[path = "files/native.rs"]
mod platform;
pub(super) use platform::*;
