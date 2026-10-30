use super::*;
use crate::platform::{Architecture, OperatingSystem};
use serde_json::json;
use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
};

fn target(os: OperatingSystem) -> Platform {
    Platform {
        os,
        arch: Architecture::X86_64,
    }
}
fn fixture() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/native-archive-tests");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
fn options() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
}
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, options()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn central_offsets(bytes: &[u8]) -> Vec<usize> {
    bytes
        .windows(4)
        .enumerate()
        .filter_map(|(offset, value)| (value == b"PK\x01\x02").then_some(offset))
        .collect()
}
fn declared_sizes(bytes: &mut [u8], sizes: &[u32]) {
    let offsets = central_offsets(bytes);
    assert_eq!(offsets.len(), sizes.len());
    for (offset, size) in offsets.into_iter().zip(sizes) {
        bytes[offset + 24..offset + 28].copy_from_slice(&size.to_le_bytes());
    }
}
fn collect(
    bytes: Vec<u8>,
    platform: Platform,
    excludes: &[Value],
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut output = Vec::new();
    extract_natives_with(
        Cursor::new(bytes),
        excludes,
        platform,
        |name, size, reader| {
            let mut bytes = Vec::new();
            let count = reader.read_to_end(&mut bytes).map_err(crate::err)? as u64;
            assert_eq!(count, size);
            output.push((name.into(), bytes));
            Ok(count)
        },
    )?;
    Ok(output)
}

#[test]
fn stream_filters_all_platform_formats_and_flattens_without_directory_entries() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in [
        "nested/lib.so",
        "nested/lib.so.1.2",
        "lib.so.sha1",
        "lib.so.bad",
        "nested/a.DLL",
        "a.dll.sha1",
        "nested/lib.dylib",
        "lib.jnilib",
        "META-INF/excluded.dll",
        "META-INF/excluded.so",
    ] {
        zip.start_file(name, options()).unwrap();
        zip.write_all(b"native").unwrap();
    }
    for name in ["directory.so/", "directory.dll/", "directory.dylib/"] {
        zip.add_directory(name, options()).unwrap();
    }
    zip.add_symlink("linked.so", "nested/lib.so", options())
        .unwrap();
    zip.add_symlink("linked.dll", "nested/a.DLL", options())
        .unwrap();
    zip.add_symlink("linked.dylib", "nested/lib.dylib", options())
        .unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    for (os, names) in [
        (OperatingSystem::Linux, vec!["lib.so", "lib.so.1.2"]),
        (OperatingSystem::Windows, vec!["a.DLL"]),
        (OperatingSystem::MacOs, vec!["lib.dylib", "lib.jnilib"]),
    ] {
        let output = collect(bytes.clone(), target(os), &[json!("META-INF/")]).unwrap();
        assert_eq!(
            output
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            names
        );
        assert!(output.iter().all(|(_, bytes)| bytes == b"native"));
    }
}
#[test]
fn unsafe_paths_fail_preflight_and_excluded_prefix_retains_legacy_policy() {
    for name in ["../escape.so", "/absolute.so", "nested/../../escape.so"] {
        let bytes = archive(&[("valid.so", b"valid"), (name, b"unsafe")]);
        let mut called = false;
        assert!(extract_natives_with(
            Cursor::new(bytes),
            &[],
            target(OperatingSystem::Linux),
            |_, _, _| {
                called = true;
                Ok(0)
            }
        )
        .is_err());
        assert!(!called, "invalid metadata must fail before any output");
    }
    assert_eq!(
        collect(
            archive(&[("META-INF/../../excluded.so", b"ignored"), ("ok.so", b"ok")]),
            target(OperatingSystem::Linux),
            &[json!("META-INF/")]
        )
        .unwrap(),
        [("ok.so".into(), b"ok".to_vec())]
    );
}
#[test]
fn short_consumers_false_counts_and_ignored_reader_errors_fail() {
    let bytes = archive(&[("lib.so", b"native")]);
    assert!(extract_natives_with(
        Cursor::new(&bytes),
        &[],
        target(OperatingSystem::Linux),
        |_, _, reader| {
            reader.read_exact(&mut [0; 1]).map_err(crate::err)?;
            Ok(1)
        }
    )
    .is_err());
    assert!(extract_natives_with(
        Cursor::new(&bytes),
        &[],
        target(OperatingSystem::Linux),
        |_, size, _| Ok(size)
    )
    .is_err());
    assert!(extract_natives_with(
        Cursor::new(&bytes),
        &[],
        target(OperatingSystem::Linux),
        |_, _, reader| { Ok(io::copy(reader, &mut io::sink()).map_err(crate::err)? + 1) }
    )
    .is_err());
    let mut bad = bytes.clone();
    let index = bad.windows(6).position(|bytes| bytes == b"native").unwrap();
    bad[index] ^= 1;
    assert!(extract_natives_with(
        Cursor::new(bad),
        &[],
        target(OperatingSystem::Linux),
        |_, size, reader| {
            let _ = io::copy(reader, &mut io::sink());
            Ok(size)
        }
    )
    .is_err());
}
#[test]
fn read_exact_cannot_skip_crc_or_zero_length_validation() {
    let mut bytes = archive(&[("lib.so", b"native")]);
    let index = bytes
        .windows(6)
        .position(|bytes| bytes == b"native")
        .unwrap();
    bytes[index] ^= 1;
    let mut completed = false;
    assert!(extract_natives_with(
        Cursor::new(bytes),
        &[],
        target(OperatingSystem::Linux),
        |_, size, reader| {
            reader
                .read_exact(&mut vec![0; size as usize])
                .map_err(crate::err)?;
            completed = true;
            Ok(size)
        }
    )
    .is_err());
    assert!(!completed);
    let mut bytes = archive(&[("empty.so", b"")]);
    let offset = central_offsets(&bytes)[0];
    bytes[offset + 16..offset + 20].copy_from_slice(&1u32.to_le_bytes());
    assert!(extract_natives_with(
        Cursor::new(bytes),
        &[],
        target(OperatingSystem::Linux),
        |_, size, _| {
            completed = true;
            Ok(size)
        }
    )
    .is_err());
    assert!(!completed, "empty CRC must be verified before emit");
    assert_eq!(
        collect(
            archive(&[("empty.so", b"")]),
            target(OperatingSystem::Linux),
            &[]
        )
        .unwrap(),
        [("empty.so".into(), Vec::new())]
    );
}
#[test]
fn declared_size_both_above_and_below_payload_is_rejected() {
    for size in [2, 9] {
        let mut bytes = archive(&[("lib.so", b"native")]);
        declared_sizes(&mut bytes, &[size]);
        assert!(collect(bytes, target(OperatingSystem::Linux), &[]).is_err());
    }
}
#[test]
fn entry_total_and_count_limits_fail_before_emit() {
    let mut oversized = archive(&[("lib.so", b"x")]);
    declared_sizes(&mut oversized, &[(ENTRY_LIMIT + 1) as u32]);
    let mut aggregate = archive(&[
        ("one.so", b"x"),
        ("two.so", b"x"),
        ("three.so", b"x"),
        ("four.so", b"x"),
        ("five.so", b"x"),
    ]);
    declared_sizes(&mut aggregate, &[ENTRY_LIMIT as u32; 5]);
    for bytes in [oversized, aggregate] {
        let mut called = false;
        assert!(extract_natives_with(
            Cursor::new(bytes),
            &[],
            target(OperatingSystem::Linux),
            |_, _, _| {
                called = true;
                Ok(0)
            }
        )
        .is_err());
        assert!(!called);
    }
    for (count, extension) in [
        (ENTRY_COUNT_LIMIT + 1, "txt"),
        (OUTPUT_COUNT_LIMIT + 1, "so"),
    ] {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..count {
            zip.start_file(format!("{index}.{extension}"), options())
                .unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let mut called = false;
        assert!(extract_natives_with(
            Cursor::new(bytes),
            &[],
            target(OperatingSystem::Linux),
            |_, _, _| {
                called = true;
                Ok(0)
            }
        )
        .is_err());
        assert!(!called);
    }
}
#[test]
fn held_reader_survives_archive_parent_rename_and_path_replacement() {
    let fixture = fixture();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("native.jar"),
        archive(&[("lib.so", b"original")]),
    )
    .unwrap();
    let file = fs::File::open(source.join("native.jar")).unwrap();
    fs::rename(&source, fixture.path().join("moved")).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("native.jar"),
        archive(&[("lib.so", b"replacement")]),
    )
    .unwrap();
    let mut bytes = Vec::new();
    extract_natives_with(
        file,
        &[],
        target(OperatingSystem::Linux),
        |name, _, reader| {
            assert_eq!(name, "lib.so");
            Ok(reader.read_to_end(&mut bytes).map_err(crate::err)? as u64)
        },
    )
    .unwrap();
    assert_eq!(bytes, b"original");
}
#[test]
fn legacy_path_adapter_replaces_name_and_preserves_the_open_old_file() {
    let fixture = fixture();
    let archive_path = fixture.path().join("native.jar");
    let target_path = fixture.path().join("natives");
    fs::create_dir(&target_path).unwrap();
    fs::write(&archive_path, archive(&[("lib.so", b"new native")])).unwrap();
    let path = target_path.join("lib.so");
    fs::write(&path, b"old native").unwrap();
    let mut old = fs::File::open(&path).unwrap();
    let identity = crate::retained_fs::metadata(&old).unwrap().identity;
    crate::extract_natives_for(
        &archive_path,
        &target_path,
        &[],
        target(OperatingSystem::Linux),
    )
    .unwrap();
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"old native");
    assert_eq!(fs::read(&path).unwrap(), b"new native");
    assert_ne!(
        crate::retained_fs::metadata(&fs::File::open(&path).unwrap())
            .unwrap()
            .identity,
        identity
    );
    let mut corrupt = archive(&[("lib.so", b"bad native")]);
    let index = corrupt
        .windows(10)
        .position(|bytes| bytes == b"bad native")
        .unwrap();
    corrupt[index] ^= 1;
    fs::write(&archive_path, corrupt).unwrap();
    assert!(crate::extract_natives_for(
        &archive_path,
        &target_path,
        &[],
        target(OperatingSystem::Linux)
    )
    .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"new native");
    assert_eq!(fs::read_dir(&target_path).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn legacy_path_adapter_keeps_an_existing_native_memory_mapping_intact() {
    use std::os::fd::AsRawFd;
    struct Mapping(*mut libc::c_void, usize);
    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.0, self.1);
            }
        }
    }
    let fixture = fixture();
    let source = fixture.path().join("natives.jar");
    let destination = fixture.path().join("natives");
    fs::create_dir(&destination).unwrap();
    let old_bytes = b"old mapped native library";
    let path = destination.join("mapped.so");
    fs::write(&path, old_bytes).unwrap();
    let old_file = fs::File::open(&path).unwrap();
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            old_bytes.len(),
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            old_file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(address, libc::MAP_FAILED);
    let mapping = Mapping(address, old_bytes.len());
    fs::write(
        &source,
        archive(&[("nested/mapped.so", b"replacement native")]),
    )
    .unwrap();
    crate::extract_natives_for(&source, &destination, &[], target(OperatingSystem::Linux)).unwrap();
    assert_eq!(
        unsafe { std::slice::from_raw_parts(mapping.0.cast::<u8>(), mapping.1) },
        old_bytes
    );
    assert_eq!(fs::read(path).unwrap(), b"replacement native");
}
