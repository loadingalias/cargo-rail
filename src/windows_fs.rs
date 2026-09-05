//! Audited Windows filesystem operations for exact cache authority.
//!
//! This is the only production module allowed to use `unsafe`. Its crate-private
//! API keeps Win32 pointer and handle contracts out of the rest of Cargo-Rail.

use std::ffi::{OsString, c_void};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_MORE_FILES, ERROR_NOT_FOUND, ERROR_NOT_SAME_DEVICE,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, FILETIME, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TEMPORARY, FILE_BASIC_INFO,
    FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAG_OVERLAPPED, FILE_FLAG_POSIX_SEMANTICS, FILE_FLAG_SEQUENTIAL_SCAN, FILE_NAME_NORMALIZED,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo, FileDispositionInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    MAXIMUM_REPARSE_DATA_BUFFER_SIZE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    PIPE_ACCESS_DUPLEX, ReadFile, SetFileInformationByHandle, VOLUME_NAME_GUID, WriteFile,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateEventW, INFINITE, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

const FILE_SYSTEM_NAME_CAPACITY: usize = 32;
const MAX_FINAL_PATH_UNITS: u32 = 32_768;
const MAX_PATH_ARGUMENT_UNITS: usize = 32_766;
const MOUNT_POINT_PATH_UNITS: usize = MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize / size_of::<u16>();
const LOCAL_PIPE_PREFIX: &str = r"\\.\pipe\cargo-rail-acquisition-";
const LOCAL_PIPE_BUFFER_BYTES: u32 = 16 * 1024;

/// One kill-on-close Job Object owning a spawned process and all descendants.
#[derive(Debug)]
pub(crate) struct ProcessJob {
    handle: OwnedHandle,
}

/// One overlapped, byte-oriented local named-pipe connection.
///
/// All I/O stays behind safe methods that keep each buffer, event, and
/// `OVERLAPPED` value alive until Windows reports completion or cancellation.
#[derive(Debug)]
pub(crate) struct LocalNamedPipe {
    handle: OwnedHandle,
}

impl LocalNamedPipe {
    pub(crate) fn read_with_timeout(
        &mut self,
        buffer: &mut [u8],
        timeout: Option<std::time::Duration>,
    ) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = u32::try_from(buffer.len().min(u32::MAX as usize))
            .map_err(|_| invalid_data("Windows named-pipe read length exceeds 32 bits"))?;
        run_overlapped(self.raw_handle(), timeout, |overlapped| {
            // SAFETY: `self` owns an overlapped-capable duplex pipe handle;
            // `buffer` is writable for `length` bytes and outlives the complete
            // or cancelled operation; `overlapped` is initialized, unique to
            // this operation, and remains live until `run_overlapped` returns.
            unsafe {
                ReadFile(
                    self.raw_handle(),
                    buffer.as_mut_ptr(),
                    length,
                    std::ptr::null_mut(),
                    overlapped,
                )
            }
        })
        .and_then(|read| {
            usize::try_from(read).map_err(|_| invalid_data("Windows named-pipe read length exceeds usize"))
        })
    }

    pub(crate) fn write_with_timeout(&mut self, buffer: &[u8], timeout: std::time::Duration) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = u32::try_from(buffer.len().min(u32::MAX as usize))
            .map_err(|_| invalid_data("Windows named-pipe write length exceeds 32 bits"))?;
        run_overlapped(self.raw_handle(), Some(timeout), |overlapped| {
            // SAFETY: `self` owns an overlapped-capable duplex pipe handle;
            // `buffer` is readable for `length` bytes and outlives the complete
            // or cancelled operation; `overlapped` is initialized, unique to
            // this operation, and remains live until `run_overlapped` returns.
            unsafe {
                WriteFile(
                    self.raw_handle(),
                    buffer.as_ptr(),
                    length,
                    std::ptr::null_mut(),
                    overlapped,
                )
            }
        })
        .and_then(|written| {
            usize::try_from(written).map_err(|_| invalid_data("Windows named-pipe write length exceeds usize"))
        })
    }

    fn raw_handle(&self) -> HANDLE {
        self.handle.as_raw_handle().cast()
    }
}

/// One blocking-accept, overlapped-I/O local named-pipe listener.
///
/// The constructor accepts only Cargo-Rail's random endpoint namespace. The
/// first-instance flag prevents an existing process from pre-creating that
/// exact name, and every instance rejects remote clients.
#[derive(Debug)]
pub(crate) struct LocalNamedPipeListener {
    name: Vec<u16>,
    max_instances: u32,
    first: Option<LocalNamedPipe>,
}

impl LocalNamedPipeListener {
    pub(crate) fn bind(name: &str, max_instances: usize) -> io::Result<Self> {
        let name = encode_local_pipe_name(name)?;
        let max_instances = u32::try_from(max_instances)
            .ok()
            .filter(|instances| *instances > 0 && *instances <= 255)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Windows broker instance bound is invalid"))?;
        let first = create_local_pipe_instance(&name, max_instances, true)?;
        Ok(Self {
            name,
            max_instances,
            first: Some(first),
        })
    }

    pub(crate) fn accept(&mut self) -> io::Result<LocalNamedPipe> {
        let pipe = match self.first.take() {
            Some(first) => first,
            None => create_local_pipe_instance(&self.name, self.max_instances, false)?,
        };
        // Heap ownership keeps the OVERLAPPED address stable for the complete
        // asynchronous connect operation.
        let mut operation = Box::new(OverlappedOperation::new()?);
        // SAFETY: `pipe` owns an overlapped-capable server pipe; `operation`
        // owns an initialized OVERLAPPED and event that remain live through the
        // completion wait. Windows retains neither after completion.
        let connected = unsafe { ConnectNamedPipe(pipe.raw_handle(), operation.overlapped_mut()) };
        if connected == 0 {
            let error = io::Error::last_os_error();
            match windows_error_code(&error) {
                Some(ERROR_PIPE_CONNECTED) => {}
                Some(ERROR_IO_PENDING) => {
                    operation.wait(pipe.raw_handle(), None)?;
                }
                _ => return Err(error),
            }
        }
        Ok(pipe)
    }
}

/// Connect to one local Cargo-Rail broker pipe within a bounded deadline.
pub(crate) fn connect_local_named_pipe(name: &str, timeout: std::time::Duration) -> io::Result<LocalNamedPipe> {
    drop(encode_local_pipe_name(name)?);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(name)
        {
            Ok(pipe) => return Ok(LocalNamedPipe { handle: pipe.into() }),
            Err(error)
                if windows_error_code(&error)
                    .is_some_and(|code| matches!(code, ERROR_PIPE_BUSY | ERROR_FILE_NOT_FOUND))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}

fn create_local_pipe_instance(name: &[u16], max_instances: u32, first: bool) -> io::Result<LocalNamedPipe> {
    let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` is a nonempty NUL-terminated UTF-16 local pipe name with
    // no interior NUL and remains live for the call. Buffer sizes and instance
    // count are bounded above, security attributes are null for the process's
    // default descriptor, and Windows retains no pointer.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            pipe_mode,
            max_instances,
            LOCAL_PIPE_BUFFER_BYTES,
            LOCAL_PIPE_BUFFER_BYTES,
            5_000,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is the unique valid server-end handle returned above;
    // ownership transfers exactly once and closes on drop.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
    Ok(LocalNamedPipe { handle })
}

struct OverlappedOperation {
    _event: OwnedHandle,
    overlapped: OVERLAPPED,
}

impl OverlappedOperation {
    fn new() -> io::Result<Self> {
        // SAFETY: all optional pointers are null, so Windows creates one
        // unnamed manual-reset event and retains no caller-owned memory.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `event` is the unique valid event handle returned above and
        // ownership transfers exactly once to `OwnedHandle`.
        let event = unsafe { OwnedHandle::from_raw_handle(event.cast()) };
        let overlapped = OVERLAPPED {
            hEvent: event.as_raw_handle().cast(),
            ..OVERLAPPED::default()
        };
        Ok(Self {
            _event: event,
            overlapped,
        })
    }

    fn overlapped_mut(&mut self) -> *mut OVERLAPPED {
        &raw mut self.overlapped
    }

    fn wait(&mut self, handle: HANDLE, timeout: Option<std::time::Duration>) -> io::Result<u32> {
        // SAFETY: the event handle is owned by `self` and remains live for the
        // wait. The call retains no handle or pointer.
        let waited = unsafe { WaitForSingleObject(self.overlapped.hEvent, wait_milliseconds(timeout)) };
        if waited == WAIT_OBJECT_0 {
            return self.result(handle, false);
        }
        if waited == WAIT_TIMEOUT {
            // SAFETY: `handle` owns the still-live operation named by the exact
            // OVERLAPPED pointer. `self` remains pinned on this stack through
            // the cancellation drain below; Windows retains no pointer after
            // the operation reaches a terminal state.
            let cancelled = unsafe { CancelIoEx(handle, &raw const self.overlapped) };
            if cancelled == 0 {
                let error = io::Error::last_os_error();
                if windows_error_code(&error) == Some(ERROR_NOT_FOUND) {
                    // The operation won the timeout race. Preserve its bytes;
                    // returning a timeout here would desynchronize the stream.
                    return self.result(handle, true);
                }
                drop(self.result(handle, true));
                return Err(error);
            }
            drop(self.result(handle, true));
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows named-pipe operation timed out",
            ));
        }

        let error = io::Error::last_os_error();
        // SAFETY: identical to the timeout cancellation above. Draining after
        // cancellation is required before the stack OVERLAPPED or borrowed I/O
        // buffer can cease to exist.
        unsafe { CancelIoEx(handle, &raw const self.overlapped) };
        drop(self.result(handle, true));
        Err(error)
    }

    fn result(&mut self, handle: HANDLE, wait: bool) -> io::Result<u32> {
        let mut transferred = 0_u32;
        // SAFETY: `handle` owns the operation described by `self.overlapped`,
        // which remains live and exclusive for this call. `transferred` is
        // writable aligned storage, and Windows retains no pointer.
        let succeeded = unsafe {
            GetOverlappedResult(
                handle,
                &raw const self.overlapped,
                &raw mut transferred,
                i32::from(wait),
            )
        };
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(transferred)
        }
    }
}

fn run_overlapped(
    handle: HANDLE,
    timeout: Option<std::time::Duration>,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> io::Result<u32> {
    // Heap ownership keeps the OVERLAPPED address stable while Windows may
    // retain it asynchronously.
    let mut operation = Box::new(OverlappedOperation::new()?);
    if start(operation.overlapped_mut()) != 0 {
        return operation.result(handle, false);
    }
    let error = io::Error::last_os_error();
    if windows_error_code(&error) != Some(ERROR_IO_PENDING) {
        return Err(error);
    }
    operation.wait(handle, timeout)
}

fn wait_milliseconds(timeout: Option<std::time::Duration>) -> u32 {
    let Some(timeout) = timeout else {
        return INFINITE;
    };
    let milliseconds = timeout.as_nanos().div_ceil(1_000_000).max(1);
    u32::try_from(milliseconds.min(u128::from(INFINITE - 1))).unwrap_or(INFINITE - 1)
}

fn windows_error_code(error: &io::Error) -> Option<u32> {
    error.raw_os_error().and_then(|code| u32::try_from(code).ok())
}

fn encode_local_pipe_name(name: &str) -> io::Result<Vec<u16>> {
    let _suffix = name
        .strip_prefix(LOCAL_PIPE_PREFIX)
        .filter(|suffix| suffix.len() == 64)
        .filter(|suffix| {
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Windows broker pipe name is not canonical"))?;
    let mut encoded = name.encode_utf16().collect::<Vec<_>>();
    encoded.push(0);
    Ok(encoded)
}

/// Spawn `command` suspended, assign its process to a kill-on-close Job Object,
/// and resume its one primary thread only after assignment succeeds.
///
/// `std::process::Command` owns quoting, environment construction, inherited
/// standard handles, and executable lookup. The suspended-start protocol closes
/// the otherwise unavoidable race between `spawn` and `AssignProcessToJobObject`.
pub(crate) fn spawn_in_process_job(command: &mut Command) -> io::Result<(Child, ProcessJob)> {
    let job = ProcessJob::new()?;
    command.creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn()?;
    if let Err(error) = job.assign_and_resume(&child) {
        let cleanup = terminate_failed_job_spawn(&mut child, &job);
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => io::Error::new(
                error.kind(),
                format!("{error}; suspended Windows process cleanup failed: {cleanup_error}"),
            ),
        });
    }
    Ok((child, job))
}

fn terminate_failed_job_spawn(child: &mut Child, job: &ProcessJob) -> io::Result<()> {
    drop(job.terminate(1));
    drop(child.kill());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other(
                "suspended Windows process remained live after forced termination",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

impl ProcessJob {
    fn new() -> io::Result<Self> {
        // SAFETY: null security attributes and name request one unnamed Job
        // Object with default security. The returned owned handle is checked
        // before conversion and Windows retains neither null pointer.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is the unique non-null Job Object handle returned by
        // CreateJobObjectW and ownership transfers exactly once to OwnedHandle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
        let job = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let limits_size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| invalid_data("Windows Job Object limit structure exceeds 32 bits"))?;
        // SAFETY: `job` owns a live Job Object handle. `limits` is a fully
        // initialized value of the exact information-class type and remains
        // alive for the synchronous call; Windows retains no pointer.
        let succeeded = unsafe {
            SetInformationJobObject(
                job.raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast::<c_void>(),
                limits_size,
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
        // SAFETY: both handles are live and borrowed for the call. `child` was
        // created suspended, so it cannot create a descendant before the Job
        // Object assignment becomes authoritative. Windows retains no handle.
        let assigned = unsafe { AssignProcessToJobObject(self.raw_handle(), child.as_raw_handle().cast()) };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }

        let thread_id = suspended_primary_thread(child.id())?;
        // SAFETY: `thread_id` was obtained from a stable system snapshot for
        // this still-suspended child. The requested access is only sufficient
        // to resume it and the returned handle is checked before ownership.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
        if thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `thread` is the unique non-null handle returned by OpenThread
        // and ownership transfers exactly once to OwnedHandle.
        let thread = unsafe { OwnedHandle::from_raw_handle(thread.cast()) };
        // SAFETY: `thread` owns the selected child's live primary thread handle.
        // The child has not executed and no other Cargo-Rail code can mutate its
        // suspend count. Windows retains no handle.
        let previous = unsafe { ResumeThread(thread.as_raw_handle().cast()) };
        if previous == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        if previous != 1 {
            return Err(invalid_data(format!(
                "Windows Cargo primary thread had unexpected suspend count {previous}"
            )));
        }
        Ok(())
    }

    pub(crate) fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: `self` owns a live Job Object handle. TerminateJobObject does
        // not retain the handle and the numeric exit code has no pointer or
        // lifetime contract.
        let succeeded = unsafe { TerminateJobObject(self.raw_handle(), exit_code) };
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) fn is_empty(&self) -> io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let accounting_size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
            .map_err(|_| invalid_data("Windows Job Object accounting structure exceeds 32 bits"))?;
        // SAFETY: `self` owns a live Job Object handle. `accounting` is aligned
        // writable storage of the exact information-class size and Windows
        // retains neither the handle nor the output pointer.
        let succeeded = unsafe {
            QueryInformationJobObject(
                self.raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast::<c_void>(),
                accounting_size,
                std::ptr::null_mut(),
            )
        };
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(accounting.ActiveProcesses == 0)
        }
    }

    fn raw_handle(&self) -> HANDLE {
        self.handle.as_raw_handle().cast()
    }
}

fn suspended_primary_thread(process_id: u32) -> io::Result<u32> {
    // SAFETY: this call has no input pointers. The returned snapshot handle is
    // checked against INVALID_HANDLE_VALUE before ownership transfer.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `snapshot` is the unique valid handle returned above and
    // ownership transfers exactly once to OwnedHandle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.cast()) };
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>())
            .map_err(|_| invalid_data("Windows thread-entry structure exceeds 32 bits"))?,
        ..THREADENTRY32::default()
    };
    let mut matching = None;

    // SAFETY: `snapshot` owns a live thread snapshot and `entry` is writable,
    // aligned storage with its required `dwSize` initialized. Windows retains
    // neither the handle nor pointer.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle().cast(), &raw mut entry) };
    if more == 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        if entry.th32OwnerProcessID == process_id && matching.replace(entry.th32ThreadID).is_some() {
            return Err(invalid_data(
                "suspended Windows Cargo process exposed more than one primary thread",
            ));
        }
        entry.dwSize = u32::try_from(size_of::<THREADENTRY32>())
            .map_err(|_| invalid_data("Windows thread-entry structure exceeds 32 bits"))?;
        // SAFETY: the same snapshot and initialized output structure remain
        // live and uniquely borrowed for this synchronous enumeration call.
        more = unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &raw mut entry) };
        if more != 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok())
            .is_some_and(|code| code == ERROR_NO_MORE_FILES)
        {
            break;
        }
        return Err(error);
    }
    matching.ok_or_else(|| invalid_data("suspended Windows Cargo process has no primary thread"))
}

#[repr(C)]
#[expect(
    non_snake_case,
    reason = "the layout and field names match the Win32 mount-point reparse buffer contract"
)]
struct MountPointBuffer {
    ReparseTag: u32,
    ReparseDataLength: u16,
    Reserved: u16,
    SubstituteNameOffset: u16,
    SubstituteNameLength: u16,
    PrintNameOffset: u16,
    PrintNameLength: u16,
    PathBuffer: [MaybeUninit<u16>; MOUNT_POINT_PATH_UNITS],
}

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

/// Open one file for stable byte observation and later handle-bound deletion.
///
/// DELETE access is bound to the returned handle. Namespace replacement after
/// this open cannot redirect [`delete_file_by_handle`] to another file.
pub(crate) fn open_for_stable_byte_observation_and_delete(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

/// Mark exactly the file bound to `file` for deletion, then close the handle.
///
/// The caller must first validate identity, bytes, and metadata through this
/// same handle. A pathname is intentionally not accepted by this operation.
pub(crate) fn delete_file_by_handle(file: File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    let buffer_size = u32::try_from(size_of::<FILE_DISPOSITION_INFO>())
        .map_err(|_| invalid_data("Windows file disposition structure size exceeds 32 bits"))?;

    // SAFETY: `file` owns a live kernel file handle opened with DELETE access.
    // `disposition` is a fully initialized repr(C) FILE_DISPOSITION_INFO whose
    // address and exact byte size remain valid for the synchronous call. The
    // API borrows neither the handle nor the buffer after it returns.
    let succeeded = unsafe {
        SetFileInformationByHandle(
            raw_handle(&file),
            FileDispositionInfo,
            std::ptr::from_ref(&disposition).cast::<c_void>(),
            buffer_size,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(file);
    Ok(())
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

/// Retain one exact directory while permitting mutations beneath it.
///
/// Later operations may write the directory namespace, but cannot request the
/// DELETE access required to rename, delete, or replace the guarded directory
/// itself. Reparse points are opened rather than followed.
pub(crate) fn open_for_mutable_directory_guard(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

/// Open one execution-path guard while following the final reparse point.
///
/// This is used only to prove that a private directory junction still resolves
/// to the already-guarded target directory. The retained handle excludes byte
/// writes, deletion, and namespace replacement on that target.
pub(crate) fn open_for_execution_guard_following_reparse(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    options.open(path)
}

/// Create one private file whose bytes remain writable only through the
/// returned handle and readable by a later Windows image loader.
pub(crate) fn create_for_execution_copy(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .attributes(FILE_ATTRIBUTE_TEMPORARY)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    options.open(path)
}

/// Return whether a hard-link failure is precisely Windows' cross-volume
/// boundary. Other errors must remain failures rather than becoming copies.
pub(crate) fn is_cross_volume_error(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
        .is_some_and(|code| code == ERROR_NOT_SAME_DEVICE)
}

/// Create and retain an exact NTFS directory junction.
///
/// `target` must already be canonical and qualified as local NTFS by the
/// caller. The returned handle opens the reparse point itself, prevents its
/// replacement, and supports later target revalidation without a close/reopen
/// race.
pub(crate) fn create_directory_junction(target: &Path, link: &Path) -> io::Result<File> {
    let substitute_name = nt_junction_target(target)?;
    let substitute_bytes = substitute_name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| invalid_data("Windows junction target length overflowed"))?;
    let reparse_data_length = 12_usize
        .checked_add(substitute_bytes)
        .ok_or_else(|| invalid_data("Windows junction reparse length overflowed"))?;
    let input_length = reparse_data_length
        .checked_add(8)
        .ok_or_else(|| invalid_data("Windows junction input length overflowed"))?;
    if substitute_name.len().saturating_add(2) > MOUNT_POINT_PATH_UNITS
        || input_length > MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows junction target exceeds the supported reparse-point bound",
        ));
    }

    let substitute_name_length =
        u16::try_from(substitute_bytes).map_err(|_| invalid_data("Windows junction target length exceeds 16 bits"))?;
    let print_name_offset = u16::try_from(substitute_bytes + size_of::<u16>())
        .map_err(|_| invalid_data("Windows junction print-name offset exceeds 16 bits"))?;
    let mut buffer = Box::new(MountPointBuffer {
        ReparseTag: IO_REPARSE_TAG_MOUNT_POINT,
        ReparseDataLength: u16::try_from(reparse_data_length)
            .map_err(|_| invalid_data("Windows junction data length exceeds 16 bits"))?,
        Reserved: 0,
        SubstituteNameOffset: 0,
        SubstituteNameLength: substitute_name_length,
        PrintNameOffset: print_name_offset,
        PrintNameLength: 0,
        PathBuffer: [MaybeUninit::uninit(); MOUNT_POINT_PATH_UNITS],
    });
    for (destination, unit) in buffer.PathBuffer.iter_mut().zip(substitute_name.iter().copied()) {
        destination.write(unit);
    }
    buffer.PathBuffer[substitute_name.len()].write(0);
    buffer.PathBuffer[substitute_name.len() + 1].write(0);

    fs::create_dir(link)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_POSIX_SEMANTICS);
    let junction = match options.open(link) {
        Ok(junction) => junction,
        Err(error) => {
            drop(fs::remove_dir(link));
            return Err(error);
        }
    };
    let input_length =
        u32::try_from(input_length).map_err(|_| invalid_data("Windows junction input length exceeds 32 bits"))?;
    let mut bytes_returned = 0_u32;
    // SAFETY: `junction` owns the newly created directory handle. `buffer` is
    // an aligned initialized mount-point header followed by the exact number
    // of initialized UTF-16 units declared by `input_length`; Windows retains
    // neither pointer and the operation has no output buffer.
    let succeeded = unsafe {
        DeviceIoControl(
            raw_handle(&junction),
            FSCTL_SET_REPARSE_POINT,
            (&raw const *buffer).cast::<c_void>(),
            input_length,
            std::ptr::null_mut(),
            0,
            &raw mut bytes_returned,
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        let error = io::Error::last_os_error();
        drop(junction);
        drop(fs::remove_dir(link));
        return Err(error);
    }
    match directory_junction_targets(&junction, target) {
        Ok(true) => Ok(junction),
        Ok(false) => {
            drop(junction);
            drop(fs::remove_dir(link));
            Err(invalid_data(
                "Windows returned a directory junction with a different target",
            ))
        }
        Err(error) => {
            drop(junction);
            drop(fs::remove_dir(link));
            Err(error)
        }
    }
}

/// Prove that one retained mount-point handle still targets `target` exactly.
pub(crate) fn directory_junction_targets(junction: &File, target: &Path) -> io::Result<bool> {
    let expected = nt_junction_target(target)?;
    let mut buffer = Box::new(MountPointBuffer {
        ReparseTag: 0,
        ReparseDataLength: 0,
        Reserved: 0,
        SubstituteNameOffset: 0,
        SubstituteNameLength: 0,
        PrintNameOffset: 0,
        PrintNameLength: 0,
        PathBuffer: [MaybeUninit::uninit(); MOUNT_POINT_PATH_UNITS],
    });
    let mut bytes_returned = 0_u32;
    // SAFETY: `junction` owns a live reparse-point handle. `buffer` is aligned
    // writable storage of at least `MAXIMUM_REPARSE_DATA_BUFFER_SIZE` bytes;
    // the returned byte count is validated before any output field or path
    // unit is read, and Windows retains no pointer.
    let succeeded = unsafe {
        DeviceIoControl(
            raw_handle(junction),
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            (&raw mut *buffer).cast::<c_void>(),
            MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
            &raw mut bytes_returned,
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    if bytes_returned < 16
        || buffer.ReparseTag != IO_REPARSE_TAG_MOUNT_POINT
        || usize::from(buffer.ReparseDataLength).saturating_add(8)
            > usize::try_from(bytes_returned)
                .map_err(|_| invalid_data("Windows junction returned byte count exceeds usize"))?
        || !buffer.SubstituteNameOffset.is_multiple_of(2)
        || !buffer.SubstituteNameLength.is_multiple_of(2)
    {
        return Err(invalid_data("Windows junction returned malformed reparse data"));
    }
    let start = usize::from(buffer.SubstituteNameOffset) / size_of::<u16>();
    let length = usize::from(buffer.SubstituteNameLength) / size_of::<u16>();
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid_data("Windows junction target range overflowed"))?;
    let path_bytes = usize::from(buffer.ReparseDataLength)
        .checked_sub(8)
        .ok_or_else(|| invalid_data("Windows junction path data is truncated"))?;
    if end > path_bytes / size_of::<u16>() || end > MOUNT_POINT_PATH_UNITS {
        return Err(invalid_data("Windows junction target exceeds returned reparse data"));
    }
    // SAFETY: the validated returned byte count covers every selected UTF-16
    // unit, and each unit lies within the aligned `PathBuffer` allocation.
    let actual = unsafe { std::slice::from_raw_parts(buffer.PathBuffer.as_ptr().cast::<u16>().add(start), length) };
    Ok(actual == expected)
}

/// Observe identity, topology, size, attributes, and mutation times through
/// one already-open file or directory handle.
///
/// The function brackets the fallback identity query with `FileBasicInfo`
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
    let file_system_name_capacity = u32::try_from(file_system_name.len())
        .map_err(|_| invalid_data("Windows filesystem-name capacity exceeds 32 bits"))?;
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
            file_system_name_capacity,
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

/// Resolve the current volume-GUID path of one retained local file handle.
pub(crate) fn opened_path(file: &File) -> io::Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(OsString::from_wide(&final_volume_guid_path(
        file,
    )?)))
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

fn nt_junction_target(target: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(target)?;
    let path = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    if path.is_empty() || path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows junction target is empty or contains a NUL code unit",
        ));
    }

    const DOS_DEVICE_PREFIX: &[u16] = &[b'\\' as u16, b'?' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const DEVICE_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'.' as u16, b'\\' as u16];
    const UNC_PREFIX: &[u16] = &[
        b'\\' as u16,
        b'?' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];

    let mut target = Vec::with_capacity(path.len().saturating_add(UNC_PREFIX.len()));
    if path.starts_with(VERBATIM_PREFIX) || path.starts_with(DOS_DEVICE_PREFIX) {
        target.extend_from_slice(DOS_DEVICE_PREFIX);
        target.extend_from_slice(&path[VERBATIM_PREFIX.len()..]);
    } else if path.starts_with(DEVICE_PREFIX) {
        target.extend_from_slice(DOS_DEVICE_PREFIX);
        target.extend_from_slice(&path[DEVICE_PREFIX.len()..]);
    } else if path.len() > 2 && path[1] == b':' as u16 && path[2] == b'\\' as u16 {
        target.extend_from_slice(DOS_DEVICE_PREFIX);
        target.extend_from_slice(&path);
    } else if path.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        target.extend_from_slice(UNC_PREFIX);
        target.extend_from_slice(&path[2..]);
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows junction target is not an absolute drive or UNC path",
        ));
    }
    Ok(target)
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
    let information_size = u32::try_from(size_of::<FILE_BASIC_INFO>())
        .map_err(|_| invalid_data("Windows basic file information size exceeds 32 bits"))?;
    // SAFETY: `file` owns a live handle for this call. `information` is a
    // correctly aligned writable `FILE_BASIC_INFO`, its exact size is passed,
    // and Windows does not retain either the handle or output pointer.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            raw_handle(file),
            FileBasicInfo,
            (&raw mut information).cast(),
            information_size,
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
        create_directory_junction, directory_junction_targets, observe_file, open_for_execution_guard,
        open_for_execution_guard_following_reparse, open_for_mutable_directory_guard, open_for_observation,
        open_for_stable_byte_observation, prove_local_ntfs, rename_write_through,
    };
    use std::fs::{self, File};
    use std::io;
    use std::time::Duration;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "fallible Windows setup precedes the test assertions"
    )]
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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "fallible Windows setup precedes the test assertions"
    )]
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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "fallible Windows setup precedes the test assertions"
    )]
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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "fallible Windows setup precedes the test assertions"
    )]
    fn mutable_directory_guard_allows_child_mutation_but_excludes_replacement() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let guarded = directory.path().join("guarded");
        let moved = directory.path().join("moved");
        fs::create_dir(&guarded)?;

        let guard = open_for_mutable_directory_guard(&guarded)?;
        let observation = observe_file(&guard)?;
        if !local_ntfs_or_explicitly_unsupported(&guard, observation.volume_serial_number)? {
            return Ok(());
        }

        let child_directory = guarded.join("child");
        let child = child_directory.join("before");
        let renamed = child_directory.join("after");
        fs::create_dir(&child_directory)?;
        fs::write(&child, b"before")?;
        fs::write(&child, b"after")?;
        fs::rename(&child, &renamed)?;
        fs::remove_file(&renamed)?;
        fs::remove_dir(&child_directory)?;

        let error = fs::rename(&guarded, &moved)
            .expect_err("a retained directory must exclude deletion and namespace replacement");
        assert_eq!(
            error.raw_os_error(),
            Some(32),
            "directory replacement must receive ERROR_SHARING_VIOLATION"
        );
        drop(guard);
        fs::rename(&guarded, &moved)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "fallible Windows setup precedes the test assertions"
    )]
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

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "fallible Windows setup precedes the test assertions"
    )]
    fn directory_junction_retains_one_exact_guarded_target() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("target");
        let alternate = directory.path().join("alternate");
        let junction = directory.path().join("junction");
        fs::create_dir(&target)?;
        fs::create_dir(&alternate)?;
        fs::write(target.join("entry"), b"target")?;

        let target_guard = open_for_execution_guard(&target)?;
        let target_observation = observe_file(&target_guard)?;
        if !local_ntfs_or_explicitly_unsupported(&target_guard, target_observation.volume_serial_number)? {
            return Ok(());
        }

        let junction_guard = create_directory_junction(&target, &junction)?;
        assert!(directory_junction_targets(&junction_guard, &target)?);
        assert!(!directory_junction_targets(&junction_guard, &alternate)?);
        assert_eq!(fs::read(junction.join("entry"))?, b"target");
        let followed_guard = open_for_execution_guard_following_reparse(&junction)?;
        assert_eq!(observe_file(&followed_guard)?, target_observation);

        let error = fs::remove_dir(&junction).expect_err("the retained junction handle must exclude replacement");
        assert_eq!(
            error.raw_os_error(),
            Some(32),
            "junction replacement must receive ERROR_SHARING_VIOLATION"
        );
        drop(followed_guard);
        drop(junction_guard);
        fs::remove_dir(&junction)?;
        assert!(target.is_dir(), "removing the junction must preserve its target");
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
