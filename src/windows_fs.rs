//! Audited Windows filesystem operations for exact cache authority.
//!
//! This is the only production module allowed to use `unsafe`. Its crate-private
//! API keeps Win32 pointer and handle contracts out of the rest of Cargo-Rail.

use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;

use windows_sys::Win32::Foundation::{FILETIME, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetVolumeInformationByHandleW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, VOLUME_NAME_GUID,
};

const FILE_SYSTEM_NAME_CAPACITY: usize = 32;
const MAX_FINAL_PATH_UNITS: u32 = 32_768;
const MAX_PATH_ARGUMENT_UNITS: usize = 32_766;

/// One stable, handle-bound observation of a Windows filesystem entry.
///
/// Times use Windows' unsigned 100-nanosecond interval representation. The
/// observation rejects zero identity or time evidence and reparse points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileObservation {
    /// Serial number of the volume containing the entry.
    pub(crate) volume_serial_number: u64,
    /// 64-bit identifier of the entry within its volume.
    pub(crate) file_id: u64,
    /// Entry creation time.
    pub(crate) creation_time: u64,
    /// Last time entry bytes were written.
    pub(crate) last_write_time: u64,
    /// Last time entry bytes or metadata changed.
    pub(crate) change_time: u64,
    /// Win32 file attributes.
    pub(crate) file_attributes: u32,
    /// Entry size in bytes.
    pub(crate) size: u64,
    /// Number of hard links to the entry.
    pub(crate) number_of_links: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BasicObservation {
    creation_time: u64,
    last_write_time: u64,
    change_time: u64,
    file_attributes: u32,
}

/// Open one existing file or directory for handle-bound identity observation.
///
/// The handle permits concurrent writers and namespace moves so it can observe
/// live authority files such as the local-CAS lifecycle lock. Reparse points
/// are opened without following them and are then rejected by [`observe_file`].
pub(crate) fn open_for_observation(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

/// Open one existing file for a stable, handle-bound byte observation.
///
/// The handle permits other readers and namespace moves but excludes byte
/// writers until it is dropped. This kernel-enforced exclusion prevents an
/// X-to-Y-to-X byte race from escaping timestamp comparison.
pub(crate) fn open_for_stable_byte_observation(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

/// Open one existing file or directory as an execution-path guard.
///
/// Other readers remain valid, including the Windows image loader, while byte
/// writes, deletion, and namespace replacement remain excluded until the
/// handle is dropped. Callers must retain guards for both an executable and
/// its parent directory when the pathname itself is the capability.
pub(crate) fn open_for_execution_guard(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

/// Observe identity, topology, size, attributes, and mutation times through
/// one already-open file or directory handle.
///
/// The function brackets the legacy identity query with `FileBasicInfo`
/// queries. A concurrent change returns [`io::ErrorKind::WouldBlock`] instead
/// of combining fields from different filesystem moments.
pub(crate) fn observe_file(file: &File) -> io::Result<FileObservation> {
    let before = query_basic_information(file)?;
    let information = query_handle_information(file)?;
    let after = query_basic_information(file)?;
    if before != after {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Windows filesystem entry changed while it was observed",
        ));
    }

    if before.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("Windows filesystem entry is a reparse point"));
    }
    if information.dwFileAttributes != before.file_attributes
        || filetime_value(information.ftCreationTime) != before.creation_time
        || filetime_value(information.ftLastWriteTime) != before.last_write_time
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Windows filesystem entry changed between handle queries",
        ));
    }

    let volume_serial_number = u64::from(information.dwVolumeSerialNumber);
    require_nonzero(volume_serial_number, "Windows volume serial number")?;
    let file_id = join_u32(information.nFileIndexHigh, information.nFileIndexLow);
    require_nonzero(file_id, "Windows file identifier")?;
    let number_of_links = u64::from(information.nNumberOfLinks);
    require_nonzero(number_of_links, "Windows hard-link count")?;

    Ok(FileObservation {
        volume_serial_number,
        file_id,
        creation_time: before.creation_time,
        last_write_time: before.last_write_time,
        change_time: before.change_time,
        file_attributes: before.file_attributes,
        size: join_u32(information.nFileSizeHigh, information.nFileSizeLow),
        number_of_links,
    })
}

/// Prove that an already-open handle belongs to local NTFS and to the volume
/// serial recorded by [`observe_file`].
///
/// The proof combines handle-bound filesystem information with a normalized
/// volume-GUID final path. A remote share, another filesystem, missing GUID,
/// zero evidence, or mismatched serial returns an error.
pub(crate) fn prove_local_ntfs(file: &File, expected_volume_serial_number: u64) -> io::Result<()> {
    require_nonzero(expected_volume_serial_number, "expected Windows volume serial number")?;
    let expected_serial = u32::try_from(expected_volume_serial_number)
        .map_err(|_| invalid_data("expected Windows volume serial number exceeds 32 bits"))?;

    let mut serial = 0_u32;
    let mut file_system_name = [0_u16; FILE_SYSTEM_NAME_CAPACITY];
    // SAFETY: `file` owns a live handle. The optional output pointers are null;
    // `serial` and `file_system_name` are valid writable outputs of the sizes
    // passed, and Windows retains none of these arguments.
    let succeeded = unsafe {
        GetVolumeInformationByHandleW(
            raw_handle(file),
            std::ptr::null_mut(),
            0,
            &raw mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system_name.as_mut_ptr(),
            FILE_SYSTEM_NAME_CAPACITY as u32,
        )
    };
    if succeeded == 0 {
        return Err(unsupported_with_source(
            "local NTFS volume information is unavailable",
            io::Error::last_os_error(),
        ));
    }
    if serial == 0 {
        return Err(invalid_data("Windows volume information returned a zero serial number"));
    }
    if serial != expected_serial {
        return Err(invalid_data(
            "Windows volume serial number does not match the file observation",
        ));
    }

    let name_end = file_system_name
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| invalid_data("Windows filesystem name is not terminated"))?;
    if !wide_ascii_eq_ignore_case(&file_system_name[..name_end], b"NTFS") {
        return Err(unsupported("Windows filesystem is not NTFS"));
    }

    let final_path = final_volume_guid_path(file)?;
    if !has_volume_guid_prefix(&final_path) {
        return Err(unsupported(
            "Windows handle does not resolve through a local volume GUID",
        ));
    }

    Ok(())
}

/// Rename `from` to `to` and ask Windows to complete the move before returning.
///
/// When `replace` is false, an existing destination is preserved and the call
/// fails. When it is true, an existing file is replaced. The operation never
/// enables `MOVEFILE_COPY_ALLOWED`, so a cross-volume request fails instead of
/// becoming a copy-and-delete operation.
pub(crate) fn rename_write_through(from: &Path, to: &Path, replace: bool) -> io::Result<()> {
    let from = encode_path(from)?;
    let to = encode_path(to)?;
    let flags = MOVEFILE_WRITE_THROUGH | if replace { MOVEFILE_REPLACE_EXISTING } else { 0 };

    // SAFETY: both vectors are nonempty, NUL-terminated UTF-16 path buffers and
    // remain alive for the call. Neither contains an interior NUL, and Windows
    // retains neither pointer after `MoveFileExW` returns.
    let succeeded = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn encode_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut encoded = Vec::new();
    for unit in path.as_os_str().encode_wide() {
        if unit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows path contains a NUL code unit",
            ));
        }
        if encoded.len() == MAX_PATH_ARGUMENT_UNITS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows path exceeds the supported UTF-16 bound",
            ));
        }
        encoded.push(unit);
    }
    if encoded.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Windows path is empty"));
    }
    encoded.push(0);
    Ok(encoded)
}

fn final_volume_guid_path(file: &File) -> io::Result<Vec<u16>> {
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_GUID;
    // SAFETY: `file` owns a live handle. A null buffer with zero capacity asks
    // Windows for the required UTF-16 length and is retained by neither side.
    let mut required = unsafe { GetFinalPathNameByHandleW(raw_handle(file), std::ptr::null_mut(), 0, flags) };
    if required == 0 {
        return Err(unsupported_with_source(
            "Windows handle has no local volume-GUID path",
            io::Error::last_os_error(),
        ));
    }

    for _ in 0..2 {
        if required > MAX_FINAL_PATH_UNITS {
            return Err(invalid_data("Windows final path exceeds the supported bound"));
        }
        let capacity = required
            .checked_add(1)
            .ok_or_else(|| invalid_data("Windows final path length overflowed"))?;
        let mut path = vec![0_u16; capacity as usize];
        // SAFETY: `file` owns a live handle. `path` is a writable UTF-16 buffer
        // with exactly `capacity` elements, and Windows retains no pointer.
        let written = unsafe { GetFinalPathNameByHandleW(raw_handle(file), path.as_mut_ptr(), capacity, flags) };
        if written == 0 {
            return Err(unsupported_with_source(
                "Windows handle has no local volume-GUID path",
                io::Error::last_os_error(),
            ));
        }
        if written < capacity {
            path.truncate(written as usize);
            return Ok(path);
        }
        required = written;
    }

    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "Windows final path changed while its volume was observed",
    ))
}

fn has_volume_guid_prefix(path: &[u16]) -> bool {
    const LEADER: &[u8] = br"\\?\Volume{";
    const GUID_LENGTH: usize = 36;
    let closing_brace = LEADER.len() + GUID_LENGTH;
    if path.len() <= closing_brace + 1
        || !wide_ascii_eq_ignore_case(&path[..LEADER.len()], LEADER)
        || path[closing_brace] != u16::from(b'}')
        || path[closing_brace + 1] != u16::from(b'\\')
    {
        return false;
    }

    let mut any_nonzero = false;
    for (index, unit) in path[LEADER.len()..closing_brace].iter().copied().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if unit != u16::from(b'-') {
                return false;
            }
        } else if !is_ascii_hex(unit) {
            return false;
        } else if unit != u16::from(b'0') {
            any_nonzero = true;
        }
    }
    any_nonzero
}

fn is_ascii_hex(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
}

fn wide_ascii_eq_ignore_case(wide: &[u16], ascii: &[u8]) -> bool {
    wide.len() == ascii.len()
        && wide
            .iter()
            .zip(ascii)
            .all(|(left, right)| fold_ascii_case(*left) == fold_ascii_case(u16::from(*right)))
}

const fn fold_ascii_case(unit: u16) -> u16 {
    if unit >= b'A' as u16 && unit <= b'Z' as u16 {
        unit + (b'a' - b'A') as u16
    } else {
        unit
    }
}

fn query_basic_information(file: &File) -> io::Result<BasicObservation> {
    let mut information = FILE_BASIC_INFO::default();
    // SAFETY: `file` owns a live handle for this call. `information` is a
    // correctly aligned writable `FILE_BASIC_INFO`, its exact size is passed,
    // and Windows does not retain either the handle or output pointer.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            raw_handle(file),
            FileBasicInfo,
            (&raw mut information).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(BasicObservation {
        creation_time: positive_time(information.CreationTime, "Windows creation time")?,
        last_write_time: positive_time(information.LastWriteTime, "Windows last-write time")?,
        change_time: positive_time(information.ChangeTime, "Windows change time")?,
        file_attributes: information.FileAttributes,
    })
}

fn query_handle_information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle for this call. `information` is a
    // correctly aligned writable value of the API's declared output type, and
    // Windows does not retain either argument.
    let succeeded = unsafe { GetFileInformationByHandle(raw_handle(file), &raw mut information) };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(information)
    }
}

fn raw_handle(file: &File) -> HANDLE {
    file.as_raw_handle().cast()
}

const fn join_u32(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

const fn filetime_value(time: FILETIME) -> u64 {
    join_u32(time.dwHighDateTime, time.dwLowDateTime)
}

fn positive_time(value: i64, name: &str) -> io::Result<u64> {
    let value = u64::try_from(value).map_err(|_| invalid_data(format!("{name} is negative")))?;
    require_nonzero(value, name)?;
    Ok(value)
}

fn require_nonzero(value: u64, name: &str) -> io::Result<()> {
    if value == 0 {
        Err(invalid_data(format!("{name} is zero")))
    } else {
        Ok(())
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message.into())
}

fn unsupported_with_source(message: &str, source: io::Error) -> io::Error {
    unsupported(format!("{message}: {source}"))
}

#[cfg(test)]
mod tests {
    use super::{
        observe_file, open_for_observation, open_for_stable_byte_observation, prove_local_ntfs, rename_write_through,
    };
    use std::fs::{self, File};
    use std::io;
    use std::time::Duration;

    #[test]
    fn observation_handle_excludes_x_y_x_byte_races() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("input.rs");
        fs::write(&path, b"X")?;

        let before_file = open_for_stable_byte_observation(&path)?;
        let before = observe_file(&before_file)?;
        if !local_ntfs_or_explicitly_unsupported(&before_file, before.volume_serial_number)? {
            return Ok(());
        }

        let error = fs::write(&path, b"Y").expect_err("an observation handle must exclude concurrent writers");
        assert_eq!(
            error.raw_os_error(),
            Some(32),
            "a concurrent writer must receive ERROR_SHARING_VIOLATION"
        );
        assert_eq!(fs::read(&path)?, b"X");

        drop(before_file);
        fs::write(&path, b"Y")?;
        fs::write(&path, b"X")?;
        assert_eq!(fs::read(&path)?, b"X");
        Ok(())
    }

    #[test]
    fn file_id_is_stable_across_write_through_rename() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let before_path = directory.path().join("before.rmeta");
        let after_path = directory.path().join("after.rmeta");
        fs::write(&before_path, b"artifact")?;

        let before_file = open_for_stable_byte_observation(&before_path)?;
        let before = observe_file(&before_file)?;
        if !local_ntfs_or_explicitly_unsupported(&before_file, before.volume_serial_number)? {
            return Ok(());
        }
        rename_write_through(&before_path, &after_path, false)?;
        let after_file = open_for_observation(&after_path)?;
        let after = observe_file(&after_file)?;

        assert_eq!(after.volume_serial_number, before.volume_serial_number);
        assert_eq!(
            after.file_id, before.file_id,
            "a same-volume rename must preserve file identity"
        );
        assert!(!before_path.exists());
        assert_eq!(fs::read(&after_path)?, b"artifact");
        Ok(())
    }

    #[test]
    fn local_ntfs_proof_succeeds_or_reports_unsupported() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("proof");
        fs::write(&path, b"proof")?;
        let file = File::open(path)?;
        let observation = observe_file(&file)?;

        let _supported = local_ntfs_or_explicitly_unsupported(&file, observation.volume_serial_number)?;
        Ok(())
    }

    #[test]
    fn directories_have_handle_bound_change_time_and_volume_proof() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let before_file = open_for_observation(directory.path())?;
        let before = observe_file(&before_file)?;
        if !local_ntfs_or_explicitly_unsupported(&before_file, before.volume_serial_number)? {
            return Ok(());
        }
        drop(before_file);

        // FILE_BASIC_INFO has 100 ns units, but Windows may assign the same timestamp within one system-clock tick.
        std::thread::sleep(Duration::from_millis(20));
        let transient = directory.path().join("transient");
        fs::write(&transient, b"value")?;
        fs::remove_file(transient)?;

        let after_file = open_for_observation(directory.path())?;
        let after = observe_file(&after_file)?;
        assert_eq!(
            after.file_id, before.file_id,
            "the directory itself must not be replaced"
        );
        assert!(
            after.change_time > before.change_time,
            "a create/delete mutation must advance the parent directory ChangeTime: before={}, after={}",
            before.change_time,
            after.change_time
        );
        Ok(())
    }

    #[test]
    fn write_through_rename_preserves_or_replaces_destination_as_requested() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"new")?;
        fs::write(&destination, b"old")?;

        let error = rename_write_through(&source, &destination, false)
            .expect_err("a no-clobber rename must reject an existing destination");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read(&source)?, b"new");
        assert_eq!(fs::read(&destination)?, b"old");

        rename_write_through(&source, &destination, true)?;
        assert!(!source.exists());
        assert_eq!(fs::read(&destination)?, b"new");
        Ok(())
    }

    fn local_ntfs_or_explicitly_unsupported(file: &File, volume_serial_number: u64) -> io::Result<bool> {
        match prove_local_ntfs(file, volume_serial_number) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                eprintln!("local NTFS proof is unavailable on this test volume: {error}");
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}
