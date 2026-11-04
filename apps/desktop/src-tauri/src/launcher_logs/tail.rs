//! Read complete suffix lines from one already-bound stream. The captured end
//! excludes later appends; the byte floor keeps the existing 16 MiB limit.
//! Blocks are retained in reverse order and copied once, avoiding repeated
//! prepends or decoding the discarded history. Path identity checks and
//! redaction remain with the caller.
use std::io::{self, Read, Seek, SeekFrom};

const BLOCK_BYTES: usize = 8 * 1024;

pub(super) fn read(
    reader: &mut (impl Read + Seek),
    end: u64,
    lines: usize,
) -> Result<Vec<u8>, String> {
    read_window(
        reader,
        end,
        lines.clamp(100, 100_000),
        super::MAX_FILE_BYTES,
    )
}

fn read_window(
    reader: &mut (impl Read + Seek),
    end: u64,
    lines: usize,
    byte_limit: u64,
) -> Result<Vec<u8>, String> {
    let floor = end.saturating_sub(byte_limit);
    let mut position = end;
    let mut blocks = Vec::new();
    let mut delimiters = 0usize;
    while position > floor {
        let length = (position - floor).min(BLOCK_BYTES as u64) as usize;
        let start = position - length as u64;
        let mut block = vec![0; length];
        reader
            .seek(SeekFrom::Start(start))
            .map_err(|error| error.to_string())?;
        reader.read_exact(&mut block).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                "当前日志读取期间被截短，请重新读取".to_owned()
            } else {
                error.to_string()
            }
        })?;
        let mut selected = None;
        for (index, byte) in block.iter().enumerate().rev() {
            // str::lines omits the empty segment after a final newline. Every
            // other LF starts one suffix line, including empty and CRLF lines.
            if *byte == b'\n' && start + index as u64 + 1 != end {
                delimiters += 1;
                if delimiters == lines {
                    selected = Some(index + 1);
                    break;
                }
            }
        }
        blocks.push(block);
        if let Some(skip) = selected {
            return Ok(assemble(blocks, skip));
        }
        position = start;
    }
    // A hard byte cut conservatively drops its first line even when the cut
    // happens to follow a newline. Drop it before UTF-8 repair or redaction, so
    // neither a partial code point nor a credential fragment can escape.
    let skip = if floor > 0 {
        blocks
            .iter()
            .rev()
            .flatten()
            .position(|byte| *byte == b'\n')
            .map_or((end - floor) as usize, |index| index + 1)
    } else {
        0
    };
    Ok(assemble(blocks, skip))
}

fn assemble(blocks: Vec<Vec<u8>>, mut skip: usize) -> Vec<u8> {
    let length = blocks.iter().map(Vec::len).sum::<usize>() - skip;
    let mut bytes = Vec::with_capacity(length);
    for block in blocks.into_iter().rev() {
        let skipped = skip.min(block.len());
        bytes.extend_from_slice(&block[skipped..]);
        skip -= skipped;
    }
    bytes
}

pub(super) fn text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut suffix = String::with_capacity(text.len());
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            suffix.push('\n');
        }
        suffix.push_str(line);
    }
    suffix
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn reference(bytes: &[u8], end: usize, lines: usize, limit: usize) -> String {
        let start = end.saturating_sub(limit);
        let window = &bytes[start..end];
        let complete = if start > 0 {
            window
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(&[][..], |index| &window[index + 1..])
        } else {
            window
        };
        let text = String::from_utf8_lossy(complete);
        let count = text.lines().count();
        text.lines()
            .skip(count.saturating_sub(lines))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn suffix_matches_previous_algorithm_across_line_and_byte_boundaries() {
        let patterns: &[&[u8]] = &[
            b"",
            b"\n",
            b"\n\n\n",
            b"first\nsecond\nlast",
            b"first\r\n\r\nlast\r\n",
            b"first\rsecond\rlast\r",
            "中文🙂\r\n\n下一行\n最后一行".as_bytes(),
            b"\xf0\x9f\n\xff\r\nend\xe4\xb8",
        ];
        for pattern in patterns {
            let mut bytes = pattern.repeat(11);
            bytes.extend_from_slice(b"appended after captured end\n");
            for end in 0..=pattern.len() * 11 {
                for limit in [1, 2, 3, 7, 31, 128, usize::MAX] {
                    for lines in [1, 2, 3, 100] {
                        let selected =
                            read_window(&mut Cursor::new(&bytes), end as u64, lines, limit as u64)
                                .unwrap();
                        assert_eq!(
                            text(&selected),
                            reference(&bytes, end, lines, limit),
                            "pattern={pattern:?} end={end} limit={limit} lines={lines}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn blocks_preserve_utf8_crlf_and_partial_final_lines() {
        let mut bytes = "前一行🙂\r\n\r\n".repeat(20_000).into_bytes();
        bytes.extend_from_slice("最后一行\r".as_bytes());
        for end in [bytes.len(), bytes.len() - 1, bytes.len() - 2] {
            for lines in [100, 200, 1000, 100_000] {
                for limit in [BLOCK_BYTES - 1, BLOCK_BYTES, BLOCK_BYTES * 3 + 2] {
                    let selected =
                        read_window(&mut Cursor::new(&bytes), end as u64, lines, limit as u64)
                            .unwrap();
                    assert_eq!(text(&selected), reference(&bytes, end, lines, limit));
                }
            }
        }
    }

    struct CountingReader<R> {
        reader: R,
        bytes_read: usize,
    }
    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = self.reader.read(bytes)?;
            self.bytes_read += count;
            Ok(count)
        }
    }
    impl<R: Seek> Seek for CountingReader<R> {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.reader.seek(position)
        }
    }

    /// A large stream without a large backing allocation in the test itself.
    struct Repeated {
        row: &'static [u8],
        length: u64,
        position: u64,
    }
    impl Read for Repeated {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = (self.length - self.position).min(bytes.len() as u64) as usize;
            for (index, byte) in bytes[..count].iter_mut().enumerate() {
                *byte = self.row[(self.position + index as u64) as usize % self.row.len()];
            }
            self.position += count as u64;
            Ok(count)
        }
    }
    impl Seek for Repeated {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            let next = match position {
                SeekFrom::Start(position) => i128::from(position),
                SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
                SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            };
            if !(0..=i128::from(self.length)).contains(&next) {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            self.position = next as u64;
            Ok(self.position)
        }
    }

    #[test]
    fn two_hundred_lines_read_two_blocks_instead_of_sixteen_mib() {
        let row = b"[00:00:00] [Render thread/INFO]: fixture line\n";
        let end = (64 * 1024 * 1024 / row.len() * row.len()) as u64;
        let mut reader = CountingReader {
            reader: Repeated {
                row,
                length: end,
                position: 0,
            },
            bytes_read: 0,
        };
        let bytes = read(&mut reader, end, 200).unwrap();
        assert_eq!(reader.bytes_read, 2 * BLOCK_BYTES);
        assert_eq!(bytes.len(), row.len() * 200);
        // The assembly peak holds these blocks plus the result. Other stream
        // history is never allocated or UTF-8 decoded by the implementation.
        assert!(reader.bytes_read + bytes.capacity() < 32 * 1024);
        assert_eq!(text(&bytes).lines().count(), 200);
        assert_eq!(
            text(&bytes),
            std::iter::repeat(std::str::from_utf8(row).unwrap().trim_end())
                .take(200)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn huge_single_line_obeys_byte_floor_and_discards_its_fragment() {
        for row in [b"x".as_slice(), b"x\n"] {
            let end = super::super::MAX_FILE_BYTES + BLOCK_BYTES as u64 + 1;
            let mut reader = CountingReader {
                reader: Repeated {
                    row,
                    length: end,
                    position: 0,
                },
                bytes_read: 0,
            };
            let bytes = read(&mut reader, end, 100_000).unwrap();
            assert!(reader.bytes_read as u64 <= super::super::MAX_FILE_BYTES);
            if row == b"x" {
                assert!(bytes.is_empty());
                assert_eq!(reader.bytes_read as u64, super::super::MAX_FILE_BYTES);
            } else {
                assert_eq!(text(&bytes).lines().count(), 100_000);
            }
        }
    }

    #[test]
    fn captured_end_excludes_appends_and_truncation_is_an_error() {
        let initial = b"first\r\nsecond\n";
        let mut bytes = initial.to_vec();
        bytes.extend_from_slice(b"later secret must stay outside captured range\n");
        let tail = read(&mut Cursor::new(bytes), initial.len() as u64, 100).unwrap();
        assert_eq!(text(&tail), "first\nsecond");
        let error = read(&mut Cursor::new(initial), initial.len() as u64 + 1, 100).unwrap_err();
        assert!(error.contains("截短"));
    }
}
