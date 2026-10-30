//! Native-library selection and verified ZIP streams, without filesystem write
//! authority. Callers own target admission, staging, publication and recovery;
//! an error can follow earlier successful emits. Only literal basenames escape
//! this module, and each consumer receives an entry stream with bounded bytes.
use crate::{platform::Platform, retained_fs::Leaf, Result};
use serde_json::Value;
use std::io::{self, Read, Seek, SeekFrom};

const ENTRY_LIMIT: u64 = 256 * 1024 * 1024;
const OUTPUT_LIMIT: u64 = 1024 * 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 512 * 1024 * 1024;
const ENTRY_COUNT_LIMIT: usize = 16_384;
const OUTPUT_COUNT_LIMIT: usize = 4096;

struct NativeEntry {
    index: usize,
    name: String,
    size: u64,
}

/// Select the same flattened native libraries as launch preparation, but emit
/// bytes through a caller-owned narrow output port. `emit` must consume the
/// entire stream and return its byte count. Final-byte delivery verifies ZIP
/// EOF/CRC even when the consumer uses `read_exact(declared_size)` rather than
/// reading until zero; swallowed read failures and short consumers are rejected.
pub fn extract_natives_with<R: Read + Seek>(
    mut archive: R,
    excludes: &[Value],
    platform: Platform,
    mut emit: impl FnMut(&str, u64, &mut dyn Read) -> Result<u64>,
) -> Result<()> {
    if archive.seek(SeekFrom::End(0)).map_err(crate::err)? > ARCHIVE_LIMIT {
        return Err("Native archive exceeds 512 MiB".into());
    }
    archive.seek(SeekFrom::Start(0)).map_err(crate::err)?;
    let mut jar = zip::ZipArchive::new(archive).map_err(crate::err)?;
    if jar.len() > ENTRY_COUNT_LIMIT {
        return Err("Native archive has too many entries".into());
    }
    // Validate metadata and the aggregate budget before the first output. A
    // later invalid path/oversized entry must not publish a partial selection.
    let mut selected = Vec::new();
    let mut bytes = 0u64;
    for index in 0..jar.len() {
        let entry = jar.by_index(index).map_err(crate::err)?;
        if excludes
            .iter()
            .filter_map(Value::as_str)
            .any(|prefix| entry.name().starts_with(prefix))
        {
            continue;
        }
        let enclosed = entry.enclosed_name().ok_or("Unsafe native archive entry")?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
            || entry.is_dir()
        {
            continue;
        }
        let Some(name) = enclosed.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !platform.native_filename(name) {
            continue;
        }
        // The archive prefix is deliberately discarded; the port never gets a
        // directory path or an archive-controlled writer/root capability.
        Leaf::new(name).map_err(|_| "Unsafe native archive basename")?;
        let size = entry.size();
        if size > ENTRY_LIMIT {
            return Err("Native archive entry exceeds 256 MiB".into());
        }
        bytes = bytes
            .checked_add(size)
            .ok_or("Native archive output size overflow")?;
        if bytes > OUTPUT_LIMIT {
            return Err("Native archive output exceeds 1 GiB".into());
        }
        if selected.len() == OUTPUT_COUNT_LIMIT {
            return Err("Native archive has too many native libraries".into());
        }
        selected.push(NativeEntry {
            index,
            name: name.to_owned(),
            size,
        });
    }
    for selected in selected {
        let mut entry = jar.by_index(selected.index).map_err(crate::err)?;
        let mut reader = VerifiedReader {
            inner: &mut entry,
            declared: selected.size,
            bytes: 0,
            eof: false,
            failed: false,
        };
        if selected.size == 0 {
            reader.finish().map_err(crate::err)?;
        }
        let reported = emit(&selected.name, selected.size, &mut reader)?;
        if reported != selected.size || reader.bytes != selected.size || reader.failed {
            return Err("Native archive consumer did not consume the declared bytes".into());
        }
        reader.finish().map_err(crate::err)?;
    }
    Ok(())
}

struct VerifiedReader<'a> {
    inner: &'a mut dyn Read,
    declared: u64,
    bytes: u64,
    eof: bool,
    failed: bool,
}
impl VerifiedReader<'_> {
    fn next(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.inner.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
                result => return result,
            }
        }
    }
    fn finish(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("Native archive stream previously failed"));
        }
        if self.eof {
            return Ok(());
        }
        if self.bytes != self.declared || self.next(&mut [0; 1])? != 0 {
            self.failed = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Native archive entry size mismatch",
            ));
        }
        self.eof = true;
        Ok(())
    }
}
impl Read for VerifiedReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.failed {
            return Err(io::Error::other("Native archive stream previously failed"));
        }
        if self.eof {
            return Ok(0);
        }
        let limit = buffer.len().min((self.declared - self.bytes) as usize);
        if limit == 0 {
            self.finish()?;
            return Ok(0);
        }
        let count = self.next(&mut buffer[..limit])?;
        if count == 0 {
            self.failed = true;
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Native archive entry ended early",
            ));
        }
        self.bytes += count as u64;
        if self.bytes == self.declared {
            self.finish()?;
        }
        Ok(count)
    }
}

#[cfg(test)]
#[path = "native_archive_tests.rs"]
mod tests;
