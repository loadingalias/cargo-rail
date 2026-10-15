//! Search requests embedded in ELF inputs. Unknown binary search metadata keeps
//! the caller's conservative candidates; it never authorizes pruning.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use crate::error::{RailError, RailResult};

/// Collect explicit binary requests, returning true when opaque metadata requires
/// preserving every direct-input candidate.
pub(super) fn implicit_names(path: &Path, names: &mut BTreeSet<String>, remaining: &mut u64) -> RailResult<bool> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    inspect(&mut file, 0, length, names, remaining, 0)
}

fn read(file: &mut File, start: u64, length: u64, end: u64, remaining: &mut u64) -> RailResult<Vec<u8>> {
    if start.checked_add(length).is_none_or(|next| next > end)
        || length > *remaining
        || length > super::MAX_ELF_LINK_DEPENDENCY_BYTES as u64
    {
        return Err(RailError::message("ELF search metadata exceeds its input bound"));
    }
    *remaining -= length;
    let length = usize::try_from(length).map_err(|_| RailError::message("ELF search metadata is too large"))?;
    let mut bytes = vec![0; length];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn integer(bytes: &[u8], offset: usize, width: usize, little: bool) -> Option<u64> {
    let source = bytes.get(offset..offset.checked_add(width)?)?;
    let mut value = [0; 8];
    if little {
        value.get_mut(..width)?.copy_from_slice(source);
        Some(u64::from_le_bytes(value))
    } else {
        value.get_mut(8usize.checked_sub(width)?..)?.copy_from_slice(source);
        Some(u64::from_be_bytes(value))
    }
}

fn inspect(
    file: &mut File,
    start: u64,
    length: u64,
    names: &mut BTreeSet<String>,
    remaining: &mut u64,
    depth: usize,
) -> RailResult<bool> {
    if depth > 1 {
        return Ok(true);
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| RailError::message("ELF input size overflow"))?;
    let header = read(file, start, length.min(64), end, remaining)?;
    if header.starts_with(b"!<thin>\n") || header.starts_with(b"BC\xc0\xde") || header.starts_with(b"\xde\xc0\x17\x0b")
    {
        return Ok(true);
    }
    if header.starts_with(b"!<arch>\n") {
        let mut offset = start + 8;
        while offset < end {
            let member = read(file, offset, 60, end, remaining)?;
            if &member[58..] != b"`\n" {
                return Ok(true);
            }
            let decimal = |bytes: &[u8]| std::str::from_utf8(bytes).ok()?.trim().parse::<u64>().ok();
            let Some(size) = decimal(&member[48..58]) else {
                return Ok(true);
            };
            let name_size = if member.starts_with(b"#1/") {
                let Some(size) = decimal(&member[3..16]) else {
                    return Ok(true);
                };
                size
            } else {
                0
            };
            let data = offset + 60;
            let Some(next) = data.checked_add(size).and_then(|next| next.checked_add(size % 2)) else {
                return Ok(true);
            };
            if next > end || name_size > size {
                return Ok(true);
            }
            if inspect(file, data + name_size, size - name_size, names, remaining, depth + 1)? {
                return Ok(true);
            }
            offset = next;
        }
        return Ok(false);
    }
    if !header.starts_with(b"\x7fELF") {
        return Ok(false);
    }
    let (wide, little) = match (header.get(4), header.get(5)) {
        (Some(1), Some(1)) => (false, true),
        (Some(1), Some(2)) => (false, false),
        (Some(2), Some(1)) => (true, true),
        (Some(2), Some(2)) => (true, false),
        _ => return Ok(true),
    };
    let (word, section_offset, entry_offset, count_offset, section_size) =
        if wide { (8, 40, 58, 60, 64) } else { (4, 32, 46, 48, 40) };
    let (Some(offset), Some(entry), Some(count)) = (
        integer(&header, section_offset, word, little),
        integer(&header, entry_offset, 2, little),
        integer(&header, count_offset, 2, little),
    ) else {
        return Ok(true);
    };
    // Extended or stripped section tables do not prove the absence of searches.
    if count == 0 || entry != section_size || offset > length {
        return Ok(true);
    }
    let sections = read(file, start + offset, count * entry, end, remaining)?;
    let entry = usize::try_from(entry).map_err(|_| RailError::message("ELF section size is invalid"))?;
    for section in sections.chunks_exact(entry) {
        match integer(section, 4, 4, little) {
            // LLVM linker options, dependent libraries and fat LTO can request
            // an archive also passed directly. Keep their candidates conservative.
            Some(0x6fff_4c01 | 0x6fff_4c04 | 0x6fff_4c0c) => return Ok(true),
            Some(6) if integer(&header, 16, 2, little) == Some(3) => {}
            _ => continue,
        }
        let (file_offset, size_offset, link_offset) = if wide { (24, 32, 40) } else { (16, 20, 24) };
        let (Some(offset), Some(size), Some(link)) = (
            integer(section, file_offset, word, little),
            integer(section, size_offset, word, little),
            integer(section, link_offset, 4, little),
        ) else {
            return Ok(true);
        };
        let Some(strings) = usize::try_from(link)
            .ok()
            .and_then(|index| sections.chunks_exact(entry).nth(index))
        else {
            return Ok(true);
        };
        let (Some(strings_offset), Some(strings_size)) = (
            integer(strings, file_offset, word, little),
            integer(strings, size_offset, word, little),
        ) else {
            return Ok(true);
        };
        if offset > length || strings_offset > length || size % (2 * word as u64) != 0 {
            return Ok(true);
        }
        let dynamic = read(file, start + offset, size, end, remaining)?;
        let strings = read(file, start + strings_offset, strings_size, end, remaining)?;
        for item in dynamic.chunks_exact(2 * word) {
            if integer(item, 0, word, little) != Some(1) {
                continue;
            }
            let Some(offset) = integer(item, word, word, little).and_then(|offset| usize::try_from(offset).ok()) else {
                return Ok(true);
            };
            let Some(tail) = strings.get(offset..) else {
                return Ok(true);
            };
            let Some(end) = tail.iter().position(|byte| *byte == 0) else {
                return Ok(true);
            };
            let Ok(name) = std::str::from_utf8(&tail[..end]) else {
                return Ok(true);
            };
            let Some(name) = Path::new(name).file_name().and_then(|name| name.to_str()) else {
                return Ok(true);
            };
            names.insert(name.to_string());
            if names.len() > super::MAX_LINK_INPUTS {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // One section table containing a dynamic section and its string table.
    fn elf(wide: bool, little: bool) -> Vec<u8> {
        let mut bytes = vec![0; 512];
        bytes[..7].copy_from_slice(&[
            0x7f,
            b'E',
            b'L',
            b'F',
            if wide { 2 } else { 1 },
            if little { 1 } else { 2 },
            1,
        ]);
        let mut put = |offset: usize, width: usize, value: u64| {
            let encoded = if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            bytes[offset..offset + width].copy_from_slice(if little {
                &encoded[..width]
            } else {
                &encoded[8 - width..]
            });
        };
        put(16, 2, 3); // ET_DYN
        if wide {
            put(40, 8, 64);
            put(58, 2, 64);
            put(60, 2, 3);
            put(132, 4, 6); // SHT_DYNAMIC
            put(152, 8, 320);
            put(160, 8, 32);
            put(168, 4, 2);
            put(196, 4, 3); // SHT_STRTAB
            put(216, 8, 400);
            put(224, 8, 16);
            put(320, 8, 1); // DT_NEEDED
            put(328, 8, 1);
        } else {
            put(32, 4, 64);
            put(46, 2, 40);
            put(48, 2, 3);
            put(108, 4, 6);
            put(120, 4, 320);
            put(124, 4, 16);
            put(128, 4, 2);
            put(148, 4, 3);
            put(160, 4, 400);
            put(164, 4, 16);
            put(320, 4, 1);
            put(324, 4, 1);
        }
        bytes[400..414].copy_from_slice(b"\0libneeded.so\0");
        bytes
    }

    #[test]
    fn elf_search_reads_needed_names_in_both_classes_and_byte_orders() {
        let root = tempfile::tempdir().expect("ELF inputs");
        let path = root.path().join("input.so");
        for wide in [false, true] {
            for little in [false, true] {
                std::fs::write(&path, elf(wide, little)).expect("shared object");
                let mut names = BTreeSet::new();
                let mut remaining = super::super::MAX_LINK_BYTES;
                assert!(!implicit_names(&path, &mut names, &mut remaining).expect("ELF search metadata"));
                assert_eq!(names, BTreeSet::from(["libneeded.so".to_string()]));
            }
        }
    }

    #[test]
    fn elf_search_retains_candidates_for_embedded_or_opaque_requests() {
        let root = tempfile::tempdir().expect("ELF inputs");
        let path = root.path().join("input");
        let mut object = elf(true, true);
        object[16..18].copy_from_slice(&1u16.to_le_bytes());
        std::fs::write(&path, &object).expect("ordinary object");
        let mut remaining = super::super::MAX_LINK_BYTES;
        let mut names = BTreeSet::new();
        assert!(!implicit_names(&path, &mut names, &mut remaining).expect("no implicit searches"));
        assert!(names.is_empty());
        object[132..136].copy_from_slice(&0x6fff_4c04u32.to_le_bytes());
        let mut archive = b"!<arch>\n".to_vec();
        archive.extend_from_slice(
            format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                "input.o/",
                0,
                0,
                0,
                644,
                object.len()
            )
            .as_bytes(),
        );
        archive.extend_from_slice(&object);
        for bytes in [
            object,
            archive,
            b"!<thin>\n".to_vec(),
            b"BC\xc0\xde".to_vec(),
            b"\xde\xc0\x17\x0b".to_vec(),
        ] {
            std::fs::write(&path, bytes).expect("input");
            let mut remaining = super::super::MAX_LINK_BYTES;
            assert!(implicit_names(&path, &mut BTreeSet::new(), &mut remaining).expect("conservative search metadata"));
        }
    }

    #[test]
    fn elf_search_bounds_metadata_reads() {
        let root = tempfile::tempdir().expect("ELF inputs");
        let path = root.path().join("input.so");
        std::fs::write(&path, elf(true, true)).expect("shared object");
        assert_eq!(
            implicit_names(&path, &mut BTreeSet::new(), &mut 63)
                .expect_err("header exceeds budget")
                .to_string(),
            "ELF search metadata exceeds its input bound"
        );
        let mut bytes = elf(true, true);
        bytes[152..160].copy_from_slice(&500u64.to_le_bytes());
        std::fs::write(&path, bytes).expect("truncated dynamic section");
        let mut remaining = super::super::MAX_LINK_BYTES;
        assert_eq!(
            implicit_names(&path, &mut BTreeSet::new(), &mut remaining)
                .expect_err("section exceeds file")
                .to_string(),
            "ELF search metadata exceeds its input bound"
        );
    }
}
