//! Read-only cache reuse for isolated installs. Verified bytes are copied into
//! the destination; processor workspaces can never mutate source-cache files.
use super::*;
use pcl_core::retained_fs::{Directory, Leaf, Links, Traversal};

pub(crate) struct CacheSource {
    root: Directory,
}
impl CacheSource {
    pub fn open(path: &Path) -> Result<Self> {
        // Canonicalizing the explicitly selected cache is intentional: a user
        // may select an alias. Every subsequent lookup stays on native retained
        // directories and rejects symlink/reparse descendants without following
        // a reconstructed parent path, including after a directory is renamed.
        let path = fs::canonicalize(path).map_err(error)?;
        let root = Directory::open_absolute(&path).map_err(error)?;
        Ok(Self { root })
    }
    fn file(&self, relative: &str) -> Result<Option<fs::File>> {
        if !(relative.starts_with("libraries/") || relative.starts_with("assets/"))
            || relative.contains(['\\', '\0'])
        {
            return Ok(None);
        }
        let parts: Vec<_> = Path::new(relative).components().collect();
        if parts.len() > 64
            || parts.len() < 2
            || parts
                .iter()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Ok(None);
        }
        let mut dir = self.root.clone();
        for (index, part) in parts.iter().enumerate() {
            let Ok(name) = Leaf::new(part.as_os_str()) else {
                return Ok(None);
            };
            // Missing, inaccessible, special or linked cache entries are misses;
            // fallback downloads remain subject to the installer's byte/hash
            // checks. Hard links are valid read-only sources: copy_to publishes
            // an independent copy, never grants a processor the source object.
            if index + 1 == parts.len() {
                return Ok(dir.open_regular(&name, Links::Any).ok());
            }
            let Ok(child) = dir.child(&name, Traversal::AllowMounts) else {
                return Ok(None);
            };
            dir = child;
        }
        Ok(None)
    }
    #[cfg(test)]
    pub fn copy(&self, root: &Path, d: &Download, cancel: &AtomicBool) -> Result<bool> {
        self.copy_to(&InstallDir::legacy(root.into()), d, cancel)
    }
    pub fn copy_to(&self, root: &InstallDir, d: &Download, cancel: &AtomicBool) -> Result<bool> {
        check(cancel)?;
        let Some(mut source) = self.file(&d.relative)? else {
            return Ok(false);
        };
        if source.metadata().map_err(error)?.len() != d.size {
            return Ok(false);
        }
        let path = root.file(&d.relative)?;
        let mut temp = path.output(d.size)?;
        let result = (|| {
            let mut digest = Sha1::new();
            let mut total = 0u64;
            let mut buf = [0u8; 65536];
            loop {
                check(cancel)?;
                let n = source.read(&mut buf).map_err(error)?;
                if n == 0 {
                    break;
                }
                total = total.checked_add(n as u64).ok_or("缓存文件过大")?;
                if total > d.size {
                    return Ok(false);
                }
                digest.update(&buf[..n]);
                temp.file_mut().write_all(&buf[..n]).map_err(error)?;
            }
            if total != d.size || !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(&d.hash)
            {
                return Ok(false);
            }
            check(cancel)?;
            root.file(&d.relative)?;
            Ok(true)
        })();
        match result {
            Ok(true) => {
                temp.publish()?;
                Ok(true)
            }
            Ok(false) => temp.discard().map(|_| false),
            Err(error) => Err(temp.close(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    fn fixture() -> tempfile::TempDir {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let work = project.join("work/instance-reset-cache-tests");
        fs::create_dir_all(&work).unwrap();
        tempfile::tempdir_in(work).unwrap()
    }
    fn artifact() -> Download {
        Download {
            relative: "libraries/example.jar".into(),
            url: "unused".into(),
            hash: format!("{:x}", Sha1::digest(b"cache")),
            size: 5,
        }
    }
    fn cached_bytes(cache: &CacheSource, relative: &str) -> Option<Vec<u8>> {
        let mut file = cache.file(relative).unwrap()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        Some(bytes)
    }
    #[cfg(unix)]
    fn directory_link(target: &Path, link: &Path) {
        symlink(target, link).unwrap();
    }
    #[cfg(windows)]
    fn directory_link(target: &Path, link: &Path) {
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
        use std::{ffi::c_void, ptr};
        #[link(name = "kernel32")]
        extern "system" {
            fn DeviceIoControl(
                handle: *mut c_void,
                code: u32,
                input: *const c_void,
                input_bytes: u32,
                output: *mut c_void,
                output_bytes: u32,
                returned: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }
        // A real local mount-point reparse, without shell, administrator,
        // developer mode, symbolic-link privilege or a new SDK dependency.
        fs::create_dir(link).unwrap();
        let target = fs::canonicalize(target).unwrap();
        let units = target.as_os_str().encode_wide().collect::<Vec<_>>();
        assert!(units.starts_with(&[92, 92, 63, 92]));
        assert_eq!(units.get(5), Some(&58));
        let print = &units[4..];
        let substitute = [92, 63, 63, 92]
            .into_iter()
            .chain(print.iter().copied())
            .collect::<Vec<_>>();
        let substitute_bytes = substitute.len() * 2;
        let print_bytes = print.len() * 2;
        let size = 16 + substitute_bytes + 2 + print_bytes + 2;
        assert!(size < 16 * 1024);
        let mut aligned = vec![0u32; size.div_ceil(4)];
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast::<u8>(), size) };
        bytes[..4].copy_from_slice(&0xA0000003u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&((size - 8) as u16).to_le_bytes());
        bytes[10..12].copy_from_slice(&(substitute_bytes as u16).to_le_bytes());
        bytes[12..14].copy_from_slice(&((substitute_bytes + 2) as u16).to_le_bytes());
        bytes[14..16].copy_from_slice(&(print_bytes as u16).to_le_bytes());
        for (index, unit) in substitute.iter().enumerate() {
            bytes[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let print_offset = 18 + substitute_bytes;
        for (index, unit) in print.iter().enumerate() {
            bytes[print_offset + index * 2..print_offset + index * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x02200000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(link)
            .unwrap();
        let mut returned = 0;
        assert_ne!(
            unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    0x000900a4,
                    aligned.as_ptr().cast(),
                    size as u32,
                    ptr::null_mut(),
                    0,
                    &mut returned,
                    ptr::null_mut(),
                )
            },
            0,
            "junction fixture failed: {}",
            std::io::Error::last_os_error()
        );
    }
    #[test]
    fn verified_cache_is_copied_and_processor_edits_cannot_touch_source() {
        let source = fixture();
        let target = fixture();
        fs::create_dir(source.path().join("libraries")).unwrap();
        fs::write(source.path().join("libraries/example.jar"), b"cache").unwrap();
        let cache = CacheSource::open(source.path()).unwrap();
        assert!(cache
            .copy(target.path(), &artifact(), &AtomicBool::new(false))
            .unwrap());
        assert_ne!(
            pcl_core::retained_fs::metadata(
                &fs::File::open(source.path().join("libraries/example.jar")).unwrap()
            )
            .unwrap()
            .identity,
            pcl_core::retained_fs::metadata(
                &fs::File::open(target.path().join("libraries/example.jar")).unwrap()
            )
            .unwrap()
            .identity
        );
        fs::write(target.path().join("libraries/example.jar"), b"changed").unwrap();
        assert_eq!(
            fs::read(source.path().join("libraries/example.jar")).unwrap(),
            b"cache"
        );
    }
    #[cfg(unix)]
    #[test]
    fn invalid_symlinked_and_traversing_cache_inputs_are_misses() {
        let source = fixture();
        let target = fixture();
        let outside = fixture();
        fs::create_dir(source.path().join("libraries")).unwrap();
        fs::write(source.path().join("libraries/example.jar"), b"wrong").unwrap();
        fs::write(outside.path().join("example.jar"), b"cache").unwrap();
        let cache = CacheSource::open(source.path()).unwrap();
        assert!(!cache
            .copy(target.path(), &artifact(), &AtomicBool::new(false))
            .unwrap());
        assert!(fs::read_dir(target.path().join("libraries"))
            .unwrap()
            .next()
            .is_none());
        fs::remove_file(source.path().join("libraries/example.jar")).unwrap();
        symlink(
            outside.path().join("example.jar"),
            source.path().join("libraries/example.jar"),
        )
        .unwrap();
        assert!(!cache
            .copy(target.path(), &artifact(), &AtomicBool::new(false))
            .unwrap());
        assert!(cache.file("libraries/../example.jar").unwrap().is_none());
        fs::remove_file(source.path().join("libraries/example.jar")).unwrap();
        fs::remove_dir(source.path().join("libraries")).unwrap();
        symlink(outside.path(), source.path().join("libraries")).unwrap();
        assert!(!cache
            .copy(target.path(), &artifact(), &AtomicBool::new(false))
            .unwrap());
    }
    #[test]
    fn cancelled_cache_copy_has_no_destination_or_partial_file() {
        let source = fixture();
        let target = fixture();
        fs::create_dir(source.path().join("libraries")).unwrap();
        fs::write(source.path().join("libraries/example.jar"), b"cache").unwrap();
        let cache = CacheSource::open(source.path()).unwrap();
        assert!(cache
            .copy(target.path(), &artifact(), &AtomicBool::new(true))
            .is_err());
        assert!(!target.path().join("libraries").exists());
    }

    #[test]
    fn read_source_keeps_hard_links_and_collection_depth_policy() {
        let source = fixture();
        fs::create_dir(source.path().join("libraries")).unwrap();
        fs::create_dir(source.path().join("assets")).unwrap();
        fs::write(source.path().join("original.jar"), b"cache").unwrap();
        fs::hard_link(
            source.path().join("original.jar"),
            source.path().join("libraries/example.jar"),
        )
        .unwrap();
        fs::write(source.path().join("assets/object"), b"asset").unwrap();
        let cache = CacheSource::open(source.path()).unwrap();
        assert_eq!(
            cached_bytes(&cache, "libraries/example.jar").unwrap(),
            b"cache"
        );
        assert_eq!(cached_bytes(&cache, "assets/object").unwrap(), b"asset");
        for relative in [
            "original.jar",
            "versions/a.jar",
            "libraries",
            "/libraries/example.jar",
            "libraries/../original.jar",
            "libraries/../../outside",
            "libraries\\example.jar",
            "libraries/a\0.jar",
            "libraries/missing.jar",
            "assets/missing/object",
        ] {
            assert!(
                cache.file(relative).unwrap().is_none(),
                "unexpected cache source: {relative:?}"
            );
        }
        let deep = format!(
            "libraries/{}/end.jar",
            std::iter::repeat_n("d", 62).collect::<Vec<_>>().join("/")
        );
        let path = source.path().join(&deep);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"deep").unwrap();
        assert_eq!(cached_bytes(&cache, &deep).unwrap(), b"deep");
        let too_deep = deep.replace("/end.jar", "/extra/end.jar");
        let path = source.path().join(&too_deep);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"too deep").unwrap();
        assert!(cache.file(&too_deep).unwrap().is_none());
    }
    #[test]
    fn read_source_stays_on_original_directory_after_root_rename() {
        let fixture = fixture();
        let original = fixture.path().join("cache");
        fs::create_dir_all(original.join("libraries")).unwrap();
        fs::write(original.join("libraries/example.jar"), b"original").unwrap();
        let cache = CacheSource::open(&original).unwrap();
        fs::rename(&original, fixture.path().join("moved-cache")).unwrap();
        fs::create_dir_all(original.join("libraries")).unwrap();
        fs::write(original.join("libraries/example.jar"), b"replacement").unwrap();
        assert_eq!(
            cached_bytes(&cache, "libraries/example.jar").unwrap(),
            b"original"
        );
    }
    #[test]
    fn read_source_rejects_descendant_links_but_canonicalizes_selected_alias() {
        let source = fixture();
        let outside = fixture();
        fs::create_dir(source.path().join("libraries")).unwrap();
        fs::create_dir(outside.path().join("libraries")).unwrap();
        fs::write(outside.path().join("libraries/example.jar"), b"cache").unwrap();
        directory_link(
            &outside.path().join("libraries"),
            &source.path().join("libraries/example.jar"),
        );
        directory_link(
            &outside.path().join("libraries"),
            &source.path().join("assets"),
        );
        let cache = CacheSource::open(source.path()).unwrap();
        assert!(cache.file("libraries/example.jar").unwrap().is_none());
        assert!(cache.file("assets/example.jar").unwrap().is_none());
        let alias = source.path().join("selected-cache");
        directory_link(outside.path(), &alias);
        let selected = CacheSource::open(&alias).unwrap();
        assert_eq!(
            cached_bytes(&selected, "libraries/example.jar").unwrap(),
            b"cache"
        );
    }
}
