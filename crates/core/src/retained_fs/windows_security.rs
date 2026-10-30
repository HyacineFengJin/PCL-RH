//! Windows ownership and DACL policy for retained filesystem handles.
//!
//! A stamp retains the complete queried self-relative owner/DACL descriptor;
//! a SID is never represented as a synthetic Unix uid. Private objects require
//! the effective TokenUser as owner and a protected, understood DACL granting
//! only that user, LocalSystem or BuiltinAdministrators. Unknown ACE forms fail
//! private admission. Creation supplies the descriptor atomically to the native
//! create operation, rather than tightening permissions after publication.
use std::{
    ffi::c_void,
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{self, NonNull},
};
use windows_sys::Win32::{
    Foundation::{LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN},
    Security::{
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_FILE_OBJECT,
        },
        CreateWellKnownSid, GetSecurityDescriptorControl, GetSecurityDescriptorLength,
        GetTokenInformation, IsValidSecurityDescriptor, TokenUser, WinBuiltinAdministratorsSid,
        WinLocalSystemSid, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, SE_DACL_PRESENT, SE_DACL_PROTECTED,
        SE_SELF_RELATIVE, TOKEN_QUERY, TOKEN_USER, WELL_KNOWN_SID_TYPE,
    },
    System::Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
};

const MAX_DESCRIPTOR_BYTES: usize = 128 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const RELATIVE_HEADER_BYTES: usize = 20;

pub(super) struct Security {
    pub owner: Vec<u8>,
    pub descriptor: Vec<u8>,
    pub owner_is_current: bool,
    pub private: bool,
}

/// LocalAlloc-backed self-relative descriptor. Moving this Rust owner does not
/// move the native allocation; no component points into Rust's stack or a Vec.
pub(crate) struct OwnedDescriptor {
    pointer: NonNull<c_void>,
    length: usize,
}
impl OwnedDescriptor {
    pub(crate) fn as_ptr(&self) -> *const SECURITY_DESCRIPTOR {
        self.pointer.as_ptr().cast()
    }

    fn bytes(&self) -> &[u8] {
        // The two native constructors allocate this entire self-relative SD.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr().cast(), self.length) }
    }

    // Only use with an allocation transferred by GetSecurityInfo or the SDDL
    // converter. Both APIs expressly require LocalFree, including error paths
    // after successful allocation but failed local validation.
    unsafe fn take(pointer: *mut c_void, reported_size: Option<usize>) -> io::Result<Self> {
        let pointer =
            NonNull::new(pointer).ok_or_else(|| invalid("Missing security descriptor"))?;
        let mut owned = Self { pointer, length: 0 };
        let mut control = 0u16;
        let mut revision = 0u32;
        if unsafe { IsValidSecurityDescriptor(pointer.as_ptr()) } == 0
            || unsafe {
                GetSecurityDescriptorControl(pointer.as_ptr(), &mut control, &mut revision)
            } == 0
        {
            return Err(invalid("Invalid native security descriptor"));
        }
        if control & SE_SELF_RELATIVE == 0 || revision != 1 {
            return Err(invalid("Expected a revision-one self-relative descriptor"));
        }
        let length = unsafe { GetSecurityDescriptorLength(pointer.as_ptr()) } as usize;
        if !(RELATIVE_HEADER_BYTES..=MAX_DESCRIPTOR_BYTES).contains(&length)
            || reported_size.is_some_and(|size| size != length)
        {
            return Err(invalid("Security descriptor exceeds its supported bounds"));
        }
        owned.length = length;
        Ok(owned)
    }
}
impl Drop for OwnedDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.pointer.as_ptr()) };
    }
}

pub(super) fn describe(file: &File) -> io::Result<Security> {
    let mut pointer = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut pointer,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = unsafe { OwnedDescriptor::take(pointer, None) }?;
    let user = current_user()?;
    let system = well_known_sid(WinLocalSystemSid)?;
    let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
    inspect_descriptor(descriptor.bytes(), &user, &system, &administrators)
}

/// Create a protected descriptor for a new literal child. OICI makes directory
/// children inherit only the same trusted principals; every private creation
/// still supplies its own protected descriptor, preventing ancestor widening.
pub(crate) fn private_descriptor(directory: bool) -> io::Result<OwnedDescriptor> {
    let user = current_user()?;
    let sid = sid_string(&user)?;
    let inheritance = if directory { "OICI" } else { "" };
    // All interpolated SID text comes from a bounded binary TokenUser, never
    // from a path, UI string or configuration. FA is file full access.
    let sddl = format!(
        "O:{sid}D:P(A;{inheritance};FA;;;{sid})(A;{inheritance};FA;;;SY)(A;{inheritance};FA;;;BA)"
    );
    let descriptor = descriptor_from_sddl(&sddl)?;
    let system = well_known_sid(WinLocalSystemSid)?;
    let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
    if !inspect_descriptor(descriptor.bytes(), &user, &system, &administrators)?.private {
        return Err(invalid(
            "Native private descriptor failed policy validation",
        ));
    }
    Ok(descriptor)
}

fn descriptor_from_sddl(sddl: &str) -> io::Result<OwnedDescriptor> {
    let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut pointer = ptr::null_mut();
    let mut length = 0u32;
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut pointer,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    unsafe { OwnedDescriptor::take(pointer, Some(length as usize)) }
}

fn current_user() -> io::Result<Vec<u8>> {
    let mut raw_token = ptr::null_mut();
    // Match the effective security identity if a caller is impersonating.
    // Falling back to the process is valid only when no thread token exists.
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw_token) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
            return Err(error);
        }
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    let mut needed = 0u32;
    let queried = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if queried != 0 {
        return Err(invalid("TokenUser unexpectedly fit a null buffer"));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(error);
    }
    if !(std::mem::size_of::<TOKEN_USER>()..=MAX_TOKEN_BYTES).contains(&(needed as usize)) {
        return Err(invalid("Invalid TokenUser buffer size"));
    }
    // TOKEN_USER contains a pointer, so a byte Vec is insufficiently aligned.
    let mut storage = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    let capacity = needed;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if needed > capacity || (needed as usize) < std::mem::size_of::<TOKEN_USER>() {
        return Err(invalid("TokenUser returned an invalid size"));
    }
    let token_user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let sid_offset = (token_user.User.Sid as usize)
        .checked_sub(storage.as_ptr() as usize)
        .filter(|offset| *offset >= std::mem::size_of::<TOKEN_USER>())
        .ok_or_else(|| invalid("TokenUser SID is outside its buffer"))?;
    let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast(), needed as usize) };
    Ok(sid_at(bytes, sid_offset)?.to_vec())
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
    let mut storage = [0u32; SECURITY_MAX_SID_SIZE as usize / 4];
    let mut length = SECURITY_MAX_SID_SIZE;
    if unsafe {
        CreateWellKnownSid(
            kind,
            ptr::null_mut(),
            storage.as_mut_ptr().cast(),
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length > SECURITY_MAX_SID_SIZE {
        return Err(invalid("Well-known SID exceeds its buffer"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast(), length as usize) };
    let sid = sid_at(bytes, 0)?;
    if sid.len() != bytes.len() {
        return Err(invalid("Well-known SID has trailing bytes"));
    }
    Ok(sid.to_vec())
}

// BEGIN BOUNDED POLICY: pure byte parsing, also exercised by hostile fixtures.
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn u16_at(bytes: &[u8], offset: usize) -> io::Result<u16> {
    let slice = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| invalid("Truncated security descriptor"))?;
    Ok(u16::from_le_bytes(slice.try_into().unwrap()))
}

fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let slice = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| invalid("Truncated security descriptor"))?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn sid_at(bytes: &[u8], offset: usize) -> io::Result<&[u8]> {
    let head = bytes
        .get(offset..offset.saturating_add(8))
        .ok_or_else(|| invalid("Truncated SID header"))?;
    if head[0] != 1 || head[1] > 15 {
        return Err(invalid("Unsupported SID revision or subauthority count"));
    }
    let length = 8 + usize::from(head[1]) * 4;
    bytes
        .get(offset..offset.saturating_add(length))
        .ok_or_else(|| invalid("Truncated SID subauthorities"))
}

fn sid_string(sid: &[u8]) -> io::Result<String> {
    if sid_at(sid, 0)?.len() != sid.len() {
        return Err(invalid("SID has trailing bytes"));
    }
    let authority = sid[2..8]
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    // Windows SID strings use hexadecimal for a 48-bit authority exceeding a
    // DWORD. TokenUser is normally NT authority, but encode the full format.
    let mut text = if authority > u64::from(u32::MAX) {
        format!("S-1-0x{authority:012X}")
    } else {
        format!("S-1-{authority}")
    };
    for offset in (8..sid.len()).step_by(4) {
        text.push_str(&format!("-{}", u32_at(sid, offset)?));
    }
    Ok(text)
}

fn relative_offset(bytes: &[u8], field: usize) -> io::Result<Option<usize>> {
    match u32_at(bytes, field)? as usize {
        0 => Ok(None),
        offset if offset >= RELATIVE_HEADER_BYTES && offset % 4 == 0 && offset < bytes.len() => {
            Ok(Some(offset))
        }
        _ => Err(invalid("Invalid security descriptor component offset")),
    }
}

fn inspect_descriptor(
    bytes: &[u8],
    current: &[u8],
    system: &[u8],
    administrators: &[u8],
) -> io::Result<Security> {
    if !(RELATIVE_HEADER_BYTES..=MAX_DESCRIPTOR_BYTES).contains(&bytes.len()) || bytes[0] != 1 {
        return Err(invalid("Invalid relative descriptor header"));
    }
    let control = u16_at(bytes, 2)?;
    if control & SE_SELF_RELATIVE == 0 {
        return Err(invalid("Descriptor is not self-relative"));
    }
    let owner = match relative_offset(bytes, 4)? {
        Some(offset) => sid_at(bytes, offset)?.to_vec(),
        None => Vec::new(),
    };
    let owner_is_current = !owner.is_empty() && owner == current;
    let dacl = relative_offset(bytes, 16)?;
    let private = owner_is_current
        && control & SE_DACL_PRESENT != 0
        && control & SE_DACL_PROTECTED != 0
        && match dacl {
            Some(offset) => private_acl(&bytes[offset..], current, system, administrators)?,
            None => false, // A null DACL grants everyone full access.
        };
    Ok(Security {
        owner,
        descriptor: bytes.to_vec(),
        owner_is_current,
        private,
    })
}

fn private_acl(
    bytes: &[u8],
    current: &[u8],
    system: &[u8],
    administrators: &[u8],
) -> io::Result<bool> {
    if bytes.len() < 8 {
        return Err(invalid("Truncated ACL header"));
    }
    let length = usize::from(u16_at(bytes, 2)?);
    if length < 8 || length > bytes.len() || length % 4 != 0 {
        return Err(invalid("ACL length is outside its descriptor"));
    }
    if !matches!(bytes[0], 2 | 4) || bytes[1] != 0 || u16_at(bytes, 6)? != 0 {
        return Ok(false);
    }
    let bytes = &bytes[..length];
    let mut offset = 8usize;
    for _ in 0..u16_at(bytes, 4)? {
        let header = bytes
            .get(offset..offset.saturating_add(8))
            .ok_or_else(|| invalid("Truncated access ACE"))?;
        let size = usize::from(u16_at(header, 2)?);
        if size < 16 || size % 4 != 0 {
            return Err(invalid("Invalid access ACE length"));
        }
        let ace = bytes
            .get(offset..offset.saturating_add(size))
            .ok_or_else(|| invalid("ACE exceeds its ACL"))?;
        // Only ordinary ACCESS_ALLOWED_ACE is understood. Object/callback and
        // conditional ACEs, deny ACEs, audit flags and future flags fail closed.
        // The five inheritance flags cannot introduce a different principal.
        if header[0] != 0 || header[1] & !0x1f != 0 {
            return Ok(false);
        }
        let mask = u32_at(ace, 4)?;
        const UNDERSTOOD_FILE_MASK: u32 = 0xf11f_01ff;
        if mask & !UNDERSTOOD_FILE_MASK != 0 {
            return Ok(false);
        }
        let sid = sid_at(ace, 8)?;
        if sid.len() + 8 != ace.len() {
            return Err(invalid("Access ACE SID length does not match its ACE"));
        }
        // Even inherit-only outsider grants are rejected: a private directory
        // must not authorize exposure of future children through inheritance.
        if sid != current && sid != system && sid != administrators {
            return Ok(false);
        }
        offset += size;
    }
    Ok(true)
}
// END BOUNDED POLICY.

#[cfg(test)]
mod policy_tests {
    use super::*;

    fn sid(authority: u8, subauthorities: &[u32]) -> Vec<u8> {
        let mut value = vec![1, subauthorities.len() as u8, 0, 0, 0, 0, 0, authority];
        for subauthority in subauthorities {
            value.extend_from_slice(&subauthority.to_le_bytes());
        }
        value
    }
    fn identities() -> [Vec<u8>; 3] {
        [
            sid(5, &[21, 11, 22, 33, 1001]),
            sid(5, &[18]),
            sid(5, &[32, 544]),
        ]
    }
    fn ace(kind: u8, flags: u8, sid: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind, flags];
        bytes.extend_from_slice(&((8 + sid.len()) as u16).to_le_bytes());
        bytes.extend_from_slice(&0x001f_01ffu32.to_le_bytes());
        bytes.extend_from_slice(sid);
        bytes
    }
    fn descriptor(owner: &[u8], aces: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = vec![0u8; RELATIVE_HEADER_BYTES];
        bytes[0] = 1;
        bytes[2..4].copy_from_slice(
            &(SE_SELF_RELATIVE | SE_DACL_PRESENT | SE_DACL_PROTECTED).to_le_bytes(),
        );
        bytes[4..8].copy_from_slice(&(RELATIVE_HEADER_BYTES as u32).to_le_bytes());
        bytes.extend_from_slice(owner);
        let offset = bytes.len();
        bytes[16..20].copy_from_slice(&(offset as u32).to_le_bytes());
        bytes.extend_from_slice(&[2, 0]);
        let size = 8 + aces.iter().map(Vec::len).sum::<usize>();
        bytes.extend_from_slice(&(size as u16).to_le_bytes());
        bytes.extend_from_slice(&(aces.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        for ace in aces {
            bytes.extend_from_slice(ace);
        }
        bytes
    }
    fn inspect(bytes: &[u8]) -> io::Result<Security> {
        let [current, system, admins] = identities();
        inspect_descriptor(bytes, &current, &system, &admins)
    }

    #[test]
    fn only_current_owner_and_understood_privileged_principals_are_private() {
        let identities = identities();
        let aces: Vec<_> = identities.iter().map(|sid| ace(0, 3, sid)).collect();
        let bytes = descriptor(&identities[0], &aces);
        let result = inspect(&bytes).unwrap();
        assert!(result.private);
        assert!(result.owner_is_current);
        assert_eq!(result.owner, identities[0]);
        assert_eq!(result.descriptor, bytes);
        assert!(!inspect(&descriptor(&identities[1], &aces)).unwrap().private);
        // An empty DACL grants nothing, unlike a null DACL.
        assert!(inspect(&descriptor(&identities[0], &[])).unwrap().private);
    }

    #[test]
    fn outsider_grants_are_rejected_even_when_inherited_or_inherit_only() {
        let current = &identities()[0];
        for outsider in [
            sid(1, &[0]),
            sid(5, &[11]),
            sid(5, &[32, 545]),
            sid(5, &[99]),
        ] {
            for flags in [0, 3, 8, 0x10, 0x1f] {
                let bytes = descriptor(current, &[ace(0, 0, current), ace(0, flags, &outsider)]);
                assert!(!inspect(&bytes).unwrap().private, "flags {flags}");
            }
        }
    }

    #[test]
    fn null_missing_or_unprotected_dacl_never_means_private() {
        let current = &identities()[0];
        let original = descriptor(current, &[ace(0, 0, current)]);
        for remove in [SE_DACL_PROTECTED, SE_DACL_PRESENT] {
            let mut bytes = original.clone();
            let control = u16_at(&bytes, 2).unwrap() & !remove;
            bytes[2..4].copy_from_slice(&control.to_le_bytes());
            assert!(!inspect(&bytes).unwrap().private);
        }
        let mut bytes = original;
        bytes[16..20].fill(0);
        assert!(!inspect(&bytes).unwrap().private);
        bytes[4..8].fill(0);
        let missing_owner = inspect(&bytes).unwrap();
        assert!(!missing_owner.owner_is_current && !missing_owner.private);
    }

    #[test]
    fn unsupported_ace_types_flags_and_access_bits_fail_private_admission() {
        let current = &identities()[0];
        for kind in [1, 5, 9, 11, 255] {
            assert!(
                !inspect(&descriptor(current, &[ace(kind, 0, current)]))
                    .unwrap()
                    .private
            );
        }
        for flag in [0x20, 0x40, 0x80] {
            assert!(
                !inspect(&descriptor(current, &[ace(0, flag, current)]))
                    .unwrap()
                    .private
            );
        }
        let mut unknown_mask = ace(0, 0, current);
        unknown_mask[4..8].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        assert!(
            !inspect(&descriptor(current, &[unknown_mask]))
                .unwrap()
                .private
        );
    }

    #[test]
    fn truncated_offsets_lengths_and_sid_counts_are_bounded_errors() {
        let current = &identities()[0];
        let original = descriptor(current, &[ace(0, 0, current)]);
        for length in 0..original.len() {
            assert!(inspect(&original[..length]).is_err(), "truncation {length}");
        }
        for field in [4, 16] {
            for offset in [1u32, 19, u32::MAX] {
                let mut bytes = original.clone();
                bytes[field..field + 4].copy_from_slice(&offset.to_le_bytes());
                assert!(inspect(&bytes).is_err());
            }
        }
        let mut bytes = original.clone();
        bytes[RELATIVE_HEADER_BYTES + 1] = 16;
        assert!(inspect(&bytes).is_err());
        let acl = u32_at(&original, 16).unwrap() as usize;
        for size in [0u16, 7, u16::MAX] {
            let mut bytes = original.clone();
            bytes[acl + 2..acl + 4].copy_from_slice(&size.to_le_bytes());
            assert!(inspect(&bytes).is_err());
        }
        let mut bytes = original;
        bytes[acl + 4..acl + 6].copy_from_slice(&2u16.to_le_bytes());
        assert!(inspect(&bytes).is_err());
    }

    #[test]
    fn sid_text_is_literal_and_full_width() {
        assert_eq!(
            sid_string(&sid(5, &[21, u32::MAX])).unwrap(),
            "S-1-5-21-4294967295"
        );
        let mut high_authority = sid(5, &[]);
        high_authority[2] = 1;
        assert_eq!(sid_string(&high_authority).unwrap(), "S-1-0x010000000005");
        let mut trailing = sid(5, &[18]);
        trailing.push(0);
        assert!(sid_string(&trailing).is_err());
    }

    #[test]
    fn stamp_preserves_acl_changes_even_when_owner_and_privacy_match() {
        let current = &identities()[0];
        let full_access = descriptor(current, &[ace(0, 0, current)]);
        let mut read_ace = ace(0, 0, current);
        read_ace[4..8].copy_from_slice(&0x0012_0089u32.to_le_bytes());
        let read_access = descriptor(current, &[read_ace]);
        let full = inspect(&full_access).unwrap();
        let read = inspect(&read_access).unwrap();
        assert!(full.private && read.private);
        assert_eq!(full.owner, read.owner);
        assert_ne!(full.descriptor, read.descriptor);
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use std::{os::windows::ffi::OsStrExt, path::Path};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
        },
    };

    fn fixture() -> tempfile::TempDir {
        let work = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../work/retained-fs-windows-security-tests");
        std::fs::create_dir_all(&work).unwrap();
        tempfile::tempdir_in(work).unwrap()
    }
    fn create(path: &Path, directory: bool, descriptor: &OwnedDescriptor) -> File {
        let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_ptr().cast_mut().cast(),
            bInheritHandle: 0,
        };
        if directory {
            assert_ne!(
                unsafe { CreateDirectoryW(path.as_ptr(), &attributes) },
                0,
                "{}",
                io::Error::last_os_error()
            );
        }
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                if directory { ptr::null() } else { &attributes },
                if directory { OPEN_EXISTING } else { CREATE_NEW },
                FILE_ATTRIBUTE_NORMAL
                    | FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
                ptr::null_mut(),
            )
        };
        assert_ne!(raw, INVALID_HANDLE_VALUE, "{}", io::Error::last_os_error());
        unsafe { File::from_raw_handle(raw) }
    }

    #[test]
    fn native_creation_is_private_and_descriptor_allocation_survives_moves() {
        let fixture = fixture();
        for directory in [false, true] {
            let descriptor = private_descriptor(directory).unwrap();
            let pointer = descriptor.as_ptr();
            let moved = vec![descriptor];
            assert_eq!(moved[0].as_ptr(), pointer);
            let path = fixture.path().join(if directory {
                "private-dir"
            } else {
                "private-file"
            });
            let file = create(&path, directory, &moved[0]);
            let security = describe(&file).unwrap();
            assert!(security.owner_is_current && security.private);
            assert_eq!(security.owner, current_user().unwrap());
            let repeated = describe(&file).unwrap();
            assert_eq!(security.descriptor, repeated.descriptor);
            let acl = u32_at(moved[0].bytes(), 16).unwrap() as usize;
            let flags = moved[0].bytes()[acl + 9];
            assert_eq!(flags, if directory { 3 } else { 0 });
        }
    }

    #[test]
    fn real_broad_dacl_is_recorded_but_never_admitted_as_private() {
        let fixture = fixture();
        let sid = sid_string(&current_user().unwrap()).unwrap();
        let descriptor =
            descriptor_from_sddl(&format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FR;;;WD)")).unwrap();
        let file = create(&fixture.path().join("broad-fixture"), false, &descriptor);
        let security = describe(&file).unwrap();
        assert!(security.owner_is_current);
        assert!(!security.private);
        assert!(!security.descriptor.is_empty());
    }
}
