//! Kernel observation of system calls that procedural-macro code issues without the C library.
//!
//! Crates such as `rustix` with its `linux_raw` backend enter the kernel directly, so rebinding a macro
//! image's imports cannot see those calls. Before rustc loads its first procedural macro, the loading thread
//! installs a seccomp filter. Threads and processes it creates later inherit the filter; macros run on that
//! thread or on threads it creates. The filter allows, inside the kernel:
//!
//! - system calls with no effect a result could depend on, such as memory management, locks, and I/O on
//!   descriptors that were opened through an observed path; and
//! - every system call issued from code of an image that was loaded before the filter, which is rustc and
//!   the C library. Their calls on behalf of a macro go through the rebound imports instead.
//!
//! Every other call suspends and notifies a supervisor thread in this process. The supervisor attributes the
//! call to a macro image by its instruction pointer, records it like the matching C library hook, and lets
//! it continue unchanged. A call from any other code, and a call the supervisor cannot classify, makes the
//! observation incomplete. Kernels without user notification (before Linux 5.5) observe nothing, and every
//! unit that loads a procedural macro then bypasses reuse.

use std::ffi::{c_int, c_long};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use super::{Unobservable, absolute_path_bytes, descriptor_path, record_path, record_unobservable};
use crate::native_input_protocol::NativeMacroPathAccess as Access;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(target_arch = "riscv64")]
const AUDIT_ARCH: u32 = 0xc000_00f3;
#[cfg(target_arch = "s390x")]
const AUDIT_ARCH: u32 = 0x8000_0016;
#[cfg(all(target_arch = "powerpc64", target_endian = "little"))]
const AUDIT_ARCH: u32 = 0xc000_0015;
#[cfg(all(target_arch = "powerpc64", target_endian = "big"))]
const AUDIT_ARCH: u32 = 0x8000_0015;

const BPF_LD_W_ABS: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
const BPF_JEQ_K: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
const BPF_JGT_K: u16 = (libc::BPF_JMP | libc::BPF_JGT | libc::BPF_K) as u16;
const BPF_JGE_K: u16 = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
const BPF_JSET_K: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
const BPF_RET_K: u16 = (libc::BPF_RET | libc::BPF_K) as u16;

/// Offsets into `struct seccomp_data`.
const DATA_NR: u32 = 0;
const DATA_ARCH: u32 = 4;
const DATA_IP: u32 = 8;
const DATA_ARGS: u32 = 16;

/// The low and high 32-bit words of a 64-bit field at `offset`.
const fn words(offset: u32) -> (u32, u32) {
    if cfg!(target_endian = "little") {
        (offset, offset + 4)
    } else {
        (offset + 4, offset)
    }
}

/// The filter's state: installed, not needed yet, or unavailable on this host.
static STATE: Mutex<FilterState> = Mutex::new(FilterState::NotInstalled);
/// Set when a notification arrives from code that is neither a macro image nor the C library.
static SUPERVISOR_FAILED: AtomicBool = AtomicBool::new(false);
static SUPERVISOR: OnceLock<std::sync::mpsc::Sender<OwnedFd>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    NotInstalled,
    Installed,
    Unavailable,
}

/// Whether every system call that macro code issued without the C library was observed.
pub(super) fn complete() -> bool {
    *STATE.lock().unwrap_or_else(PoisonError::into_inner) == FilterState::Installed
        && !SUPERVISOR_FAILED.load(Ordering::Acquire)
}

/// Install the filter on the calling thread, allowing calls from every executable range loaded now.
///
/// Call before rustc loads a procedural macro, on the thread that loads it.
pub(super) fn install_on_current_thread(allowed: &[(usize, usize)]) {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if *state == FilterState::Installed && installed_on_this_thread() {
        return;
    }
    *state = match install(allowed) {
        Ok(()) => FilterState::Installed,
        Err(()) => FilterState::Unavailable,
    };
}

thread_local! {
    static THREAD_FILTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn installed_on_this_thread() -> bool {
    THREAD_FILTERED.with(std::cell::Cell::get)
}

fn install(allowed: &[(usize, usize)]) -> Result<(), ()> {
    if !kernel_continues_notified_calls() {
        return Err(());
    }
    let program = filter(allowed);
    let length = u16::try_from(program.len()).map_err(|_| ())?;
    let program = libc::sock_fprog {
        len: length,
        filter: program.as_ptr().cast_mut(),
    };
    // SAFETY: prctl and seccomp read only their scalar arguments and the program, which outlives the call.
    let listener = unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(());
        }
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &program as *const libc::sock_fprog,
        )
    };
    if listener < 0 {
        return Err(());
    }
    THREAD_FILTERED.with(|filtered| filtered.set(true));
    // SAFETY: the kernel returned a new descriptor that this process owns.
    let listener = unsafe { OwnedFd::from_raw_fd(listener as c_int) };
    let supervisor = SUPERVISOR.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        // The supervisor runs code of this driver and the C library only, so the filter never suspends it.
        let spawned = std::thread::Builder::new()
            .name("cargo-rail-macro-observer".into())
            .spawn(move || supervise(receiver));
        if spawned.is_err() {
            SUPERVISOR_FAILED.store(true, Ordering::Release);
        }
        sender
    });
    supervisor.send(listener).map_err(|_| ())
}

/// `SECCOMP_USER_NOTIF_FLAG_CONTINUE` appeared in Linux 5.5; without it a notified call cannot proceed.
fn kernel_continues_notified_calls() -> bool {
    // SAFETY: uname writes only the provided structure.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return false;
    }
    let release = name
        .release
        .iter()
        .take_while(|byte| **byte != 0)
        // `c_char` is signed on some architectures and unsigned on others.
        .map(|byte| char::from(byte.to_ne_bytes()[0]))
        .collect::<String>();
    let mut numbers = release
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse::<u32>().ok());
    let (Some(major), Some(minor)) = (numbers.next(), numbers.next()) else {
        return false;
    };
    let action = libc::SECCOMP_RET_USER_NOTIF;
    // SAFETY: SECCOMP_GET_ACTION_AVAIL reads one action value.
    let available = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_GET_ACTION_AVAIL,
            0,
            &action as *const libc::c_uint,
        )
    } == 0;
    available && (major, minor) >= (5, 5)
}

fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt: 0, jf: 0, k }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// Build the filter program. Every conditional jump stays within its block, and each allowing block returns.
fn filter(allowed: &[(usize, usize)]) -> Vec<libc::sock_filter> {
    let allow = statement(BPF_RET_K, libc::SECCOMP_RET_ALLOW);
    let notify = statement(BPF_RET_K, libc::SECCOMP_RET_USER_NOTIF);
    let mut program = vec![
        statement(BPF_LD_W_ABS, DATA_ARCH),
        // Another system-call ABI in the same process, such as x86 compatibility mode, is always notified.
        jump(BPF_JEQ_K, AUDIT_ARCH, 1, 0),
        notify,
        statement(BPF_LD_W_ABS, DATA_NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        // x32 system calls set bit 30 of the number.
        program.push(jump(BPF_JGE_K, 0x4000_0000, 0, 1));
        program.push(notify);
    }
    for &number in PURE_SYSTEM_CALLS {
        program.push(jump(BPF_JEQ_K, number as u32, 0, 1));
        program.push(allow);
    }
    // Memory mappings are pure unless they create executable code.
    let (protection_low, _) = words(DATA_ARGS + 2 * 8);
    for number in MEMORY_PROTECTION {
        program.extend([
            jump(BPF_JEQ_K, number as u32, 0, 4),
            statement(BPF_LD_W_ABS, protection_low),
            jump(BPF_JSET_K, libc::PROT_EXEC as u32, 1, 0),
            allow,
            statement(BPF_LD_W_ABS, DATA_NR),
        ]);
    }
    // New threads are pure; new processes are not.
    #[cfg(target_arch = "s390x")]
    let clone_flags = DATA_ARGS + 8;
    #[cfg(not(target_arch = "s390x"))]
    let clone_flags = DATA_ARGS;
    let (clone_flags_low, _) = words(clone_flags);
    program.extend([
        jump(BPF_JEQ_K, libc::SYS_clone as u32, 0, 4),
        statement(BPF_LD_W_ABS, clone_flags_low),
        jump(BPF_JSET_K, libc::CLONE_THREAD as u32, 0, 1),
        allow,
        statement(BPF_LD_W_ABS, DATA_NR),
    ]);
    let (ip_low, ip_high) = words(DATA_IP);
    for &(start, end) in allowed {
        let (start_high, start_low) = ((start as u64 >> 32) as u32, start as u32);
        let (end_high, end_low) = ((end as u64 >> 32) as u32, end as u32);
        // start <= ip < end, compared through the 32-bit halves. Offsets count from the next instruction;
        // index 10 allows and index 11 is the next range.
        program.extend([
            /* 0 */ statement(BPF_LD_W_ABS, ip_high),
            /* 1 */ jump(BPF_JGT_K, start_high, 3, 0), // above the start's high word: check the end
            /* 2 */ jump(BPF_JEQ_K, start_high, 0, 8), // below it: next range
            /* 3 */ statement(BPF_LD_W_ABS, ip_low),
            /* 4 */ jump(BPF_JGE_K, start_low, 0, 6), // below the start: next range
            /* 5 */ statement(BPF_LD_W_ABS, ip_high),
            /* 6 */ jump(BPF_JGT_K, end_high, 4, 0), // above the end's high word: next range
            /* 7 */ jump(BPF_JEQ_K, end_high, 0, 2), // below it: allow
            /* 8 */ statement(BPF_LD_W_ABS, ip_low),
            /* 9 */ jump(BPF_JGE_K, end_low, 1, 0), // at or past the end: next range
            /* 10 */ allow,
        ]);
    }
    program.push(notify);
    program
}

/// Receive notifications from every listener and let each suspended call continue.
fn supervise(receiver: std::sync::mpsc::Receiver<OwnedFd>) {
    let mut listeners = Vec::<OwnedFd>::new();
    loop {
        while let Ok(listener) = receiver.try_recv() {
            listeners.push(listener);
        }
        if listeners.is_empty() {
            match receiver.recv() {
                Ok(listener) => listeners.push(listener),
                Err(_) => return,
            }
            continue;
        }
        let mut descriptors = listeners
            .iter()
            .map(|listener| libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect::<Vec<_>>();
        // SAFETY: poll writes only the provided array. A short timeout admits listeners of later threads.
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as libc::nfds_t, 50) };
        if ready <= 0 {
            continue;
        }
        let mut closed = Vec::new();
        for (index, descriptor) in descriptors.iter().enumerate() {
            if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                closed.push(index);
            } else if descriptor.revents & libc::POLLIN != 0 {
                respond(descriptor.fd);
            }
        }
        for index in closed.into_iter().rev() {
            listeners.remove(index);
        }
    }
}

fn respond(listener: c_int) {
    // SAFETY: the kernel fills the notification; both structures live on this stack for the calls.
    let mut notification: libc::seccomp_notif = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(listener, libc::SECCOMP_IOCTL_NOTIF_RECV, &mut notification) } != 0 {
        return;
    }
    if thread_group(notification.pid) == Some(std::process::id()) {
        let instruction = notification.data.instruction_pointer as usize;
        if super::elf::macro_image_contains(instruction) {
            observe_system_call(notification.data.nr as c_long, notification.data.args);
        } else {
            // Code that is neither rustc, the C library, nor an instrumented macro, such as generated code
            // or a library a macro loaded, issued this call.
            record_unobservable(Unobservable::RawSystemCall);
        }
    }
    // Another process, such as a linker rustc started, inherits the filter; its calls are not macro code.
    let mut response = libc::seccomp_notif_resp {
        id: notification.id,
        val: 0,
        error: 0,
        flags: libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32,
    };
    if unsafe { libc::ioctl(listener, libc::SECCOMP_IOCTL_NOTIF_SEND, &mut response) } != 0 {
        // The call ended before the response, for example because its thread received a signal.
    }
}

/// The process that owns a thread, from its status file.
fn thread_group(thread: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{thread}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Tgid:"))
        .and_then(|value| value.trim().parse().ok())
}

/// Copy a NUL-terminated string from this process without faulting on an invalid pointer.
fn read_c_string(address: u64) -> Option<Vec<u8>> {
    if address == 0 {
        return None;
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 256];
    while bytes.len() < libc::PATH_MAX as usize {
        let local = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        let remote = libc::iovec {
            iov_base: (address as usize + bytes.len()) as *mut libc::c_void,
            iov_len: buffer.len(),
        };
        // SAFETY: process_vm_readv copies into the local buffer and reports faults as errors.
        let read = unsafe { libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0) };
        if read <= 0 {
            return None;
        }
        let read = &buffer[..read as usize];
        if let Some(end) = read.iter().position(|byte| *byte == 0) {
            bytes.extend_from_slice(&read[..end]);
            return Some(bytes);
        }
        bytes.extend_from_slice(read);
    }
    None
}

fn observe_path(directory: u64, path: u64, access: Access) {
    // A call with an unreadable path fails with EFAULT and reads nothing.
    if let Some(bytes) = read_c_string(path)
        && let Some(path) = absolute_path_bytes(directory as c_int, &bytes)
    {
        record_path(path, access);
    }
}

fn observe_open(directory: u64, path: u64, flags: u64) {
    let flags = flags as c_int;
    let writes = flags & libc::O_ACCMODE != libc::O_RDONLY
        || flags & (libc::O_CREAT | libc::O_TRUNC) != 0
        || flags & libc::O_TMPFILE == libc::O_TMPFILE;
    if writes {
        record_unobservable(Unobservable::FileWrite);
    } else if flags & (libc::O_DIRECTORY | libc::O_PATH) != 0 {
        observe_path(directory, path, Access::Entry);
    } else {
        observe_path(directory, path, Access::Contents);
    }
}

fn observe_entry_at(directory: u64, path: u64, flags: u64) {
    if flags as c_int & libc::AT_EMPTY_PATH != 0 && read_c_string(path).is_some_and(|bytes| bytes.is_empty()) {
        return;
    }
    observe_path(directory, path, Access::Entry);
}

fn observe_listing(descriptor: u64) {
    match descriptor_path(descriptor as c_int) {
        Some(path) => record_path(path, Access::Listing),
        None => record_unobservable(Unobservable::PathUnavailable),
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
const AT_FDCWD: u64 = libc::AT_FDCWD as i64 as u64;

// Architectures offer different system calls. A call that no list here names is always notified,
// and `observe_system_call` bypasses a notified call that it does not classify.
/// System calls that set memory protection; `libc` names no `pkey_mprotect` on s390x or POWER.
#[cfg(not(any(target_arch = "s390x", target_arch = "powerpc64")))]
const MEMORY_PROTECTION: [c_long; 3] = [libc::SYS_mmap, libc::SYS_mprotect, libc::SYS_pkey_mprotect];
#[cfg(any(target_arch = "s390x", target_arch = "powerpc64"))]
const MEMORY_PROTECTION: [c_long; 2] = [libc::SYS_mmap, libc::SYS_mprotect];

/// Record what one system call from macro code reads, or why it cannot be bound.
pub(super) fn observe_system_call(number: c_long, a: [u64; 6]) {
    use Unobservable::{ExecutableMemory, ProcessControl, RawSystemCall};

    if PURE_SYSTEM_CALLS.contains(&number) {
        return;
    }
    if let Some(&(_, effect)) = EFFECT_SYSTEM_CALLS.iter().find(|(effect, _)| *effect == number) {
        record_unobservable(effect);
        return;
    }
    match number {
        libc::SYS_openat => observe_open(a[0], a[1], a[2]),
        libc::SYS_openat2 => {
            // `struct open_how` starts with the 64-bit flags.
            let mut flags = [0u8; 8];
            let local = libc::iovec {
                iov_base: flags.as_mut_ptr().cast(),
                iov_len: flags.len(),
            };
            let remote = libc::iovec {
                iov_base: a[2] as usize as *mut libc::c_void,
                iov_len: flags.len(),
            };
            // SAFETY: process_vm_readv copies into the local buffer and reports faults as errors.
            if unsafe { libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0) } == 8 {
                observe_open(a[0], a[1], u64::from_ne_bytes(flags));
            }
        }
        libc::SYS_newfstatat | libc::SYS_statx => {
            observe_entry_at(a[0], a[1], if number == libc::SYS_statx { a[2] } else { a[3] })
        }
        libc::SYS_faccessat | libc::SYS_faccessat2 | libc::SYS_readlinkat => observe_path(a[0], a[1], Access::Entry),
        libc::SYS_getdents64 => observe_listing(a[0]),
        number if MEMORY_PROTECTION.contains(&number) => {
            if a[2] as c_int & libc::PROT_EXEC != 0 {
                record_unobservable(ExecutableMemory);
            }
        }
        libc::SYS_clone => {
            #[cfg(target_arch = "s390x")]
            let flags = a[1];
            #[cfg(not(target_arch = "s390x"))]
            let flags = a[0];
            if flags as c_int & libc::CLONE_THREAD == 0 {
                record_unobservable(ProcessControl);
            }
        }
        _ => observe_legacy_system_call(number, a).unwrap_or_else(|| record_unobservable(RawSystemCall)),
    }
}

/// Path reads through system calls that newer architectures replaced with their `*at` forms.
#[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
fn observe_legacy_system_call(number: c_long, a: [u64; 6]) -> Option<()> {
    match number {
        libc::SYS_open => observe_open(AT_FDCWD, a[0], a[1]),
        libc::SYS_stat | libc::SYS_lstat | libc::SYS_access | libc::SYS_readlink => {
            observe_path(AT_FDCWD, a[0], Access::Entry);
        }
        libc::SYS_getdents => observe_listing(a[0]),
        _ => return None,
    }
    Some(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64")))]
fn observe_legacy_system_call(_number: c_long, _a: [u64; 6]) -> Option<()> {
    None
}

/// System calls with no effect a result could depend on. File metadata and reads here operate on
/// descriptors, which the macro could only have opened through an observed path. Event queues create
/// descriptors without a path and report only on descriptors the macro holds.
///
/// The import tables in `hooks.rs` classify each C library function as this table classifies its system
/// call; a test enforces it.
const PURE_SYSTEM_CALLS: &[c_long] = &[
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_readv,
    libc::SYS_writev,
    libc::SYS_pread64,
    libc::SYS_pwrite64,
    libc::SYS_preadv,
    libc::SYS_pwritev,
    libc::SYS_close,
    libc::SYS_fstat,
    libc::SYS_lseek,
    libc::SYS_munmap,
    libc::SYS_mremap,
    libc::SYS_madvise,
    libc::SYS_brk,
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    libc::SYS_rt_sigreturn,
    libc::SYS_sigaltstack,
    libc::SYS_futex,
    libc::SYS_sched_yield,
    libc::SYS_sched_getaffinity,
    libc::SYS_nanosleep,
    libc::SYS_clock_nanosleep,
    libc::SYS_clock_gettime,
    libc::SYS_clock_getres,
    libc::SYS_gettimeofday,
    libc::SYS_getpid,
    libc::SYS_gettid,
    libc::SYS_getppid,
    libc::SYS_getuid,
    libc::SYS_geteuid,
    libc::SYS_getgid,
    libc::SYS_getegid,
    libc::SYS_exit,
    libc::SYS_exit_group,
    libc::SYS_set_robust_list,
    libc::SYS_get_robust_list,
    libc::SYS_rseq,
    libc::SYS_set_tid_address,
    libc::SYS_getrandom,
    libc::SYS_membarrier,
    libc::SYS_prlimit64,
    libc::SYS_dup,
    libc::SYS_dup3,
    libc::SYS_pipe2,
    libc::SYS_fcntl,
    libc::SYS_ioctl,
    libc::SYS_ppoll,
    libc::SYS_pselect6,
    libc::SYS_wait4,
    libc::SYS_waitid,
    libc::SYS_kill,
    libc::SYS_tgkill,
    libc::SYS_tkill,
    libc::SYS_restart_syscall,
    libc::SYS_getcwd,
    libc::SYS_getrusage,
    libc::SYS_fsync,
    libc::SYS_fdatasync,
    libc::SYS_flock,
    libc::SYS_sendfile,
    libc::SYS_splice,
    libc::SYS_pidfd_open,
    libc::SYS_pidfd_send_signal,
    libc::SYS_eventfd2,
    libc::SYS_epoll_create1,
    libc::SYS_epoll_ctl,
    libc::SYS_epoll_pwait,
    libc::SYS_epoll_pwait2,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_arch_prctl,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_poll,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_dup2,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_pipe,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_pause,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_eventfd,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_epoll_create,
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    libc::SYS_epoll_wait,
];

/// System calls whose every use is an effect that no key can bind, and the effect each one records.
const EFFECT_SYSTEM_CALLS: &[(c_long, Unobservable)] = &[
    (libc::SYS_chdir, Unobservable::WorkingDirectory),
    (libc::SYS_fchdir, Unobservable::WorkingDirectory),
    (libc::SYS_chroot, Unobservable::ProcessControl),
    (libc::SYS_execve, Unobservable::ProcessControl),
    (libc::SYS_execveat, Unobservable::ProcessControl),
    (libc::SYS_clone3, Unobservable::ProcessControl),
    (libc::SYS_setuid, Unobservable::ProcessControl),
    (libc::SYS_setgid, Unobservable::ProcessControl),
    (libc::SYS_setgroups, Unobservable::ProcessControl),
    (libc::SYS_setpgid, Unobservable::ProcessControl),
    (libc::SYS_setsid, Unobservable::ProcessControl),
    // A socket can carry data from any peer, and `recvmsg` can receive another process's descriptors.
    (libc::SYS_socket, Unobservable::Network),
    (libc::SYS_socketpair, Unobservable::Network),
    (libc::SYS_connect, Unobservable::Network),
    (libc::SYS_bind, Unobservable::Network),
    (libc::SYS_listen, Unobservable::Network),
    #[cfg(not(target_arch = "s390x"))]
    (libc::SYS_accept, Unobservable::Network),
    (libc::SYS_accept4, Unobservable::Network),
    (libc::SYS_sendto, Unobservable::Network),
    (libc::SYS_sendmsg, Unobservable::Network),
    (libc::SYS_sendmmsg, Unobservable::Network),
    (libc::SYS_recvfrom, Unobservable::Network),
    (libc::SYS_recvmsg, Unobservable::Network),
    (libc::SYS_recvmmsg, Unobservable::Network),
    (libc::SYS_getsockopt, Unobservable::Network),
    (libc::SYS_setsockopt, Unobservable::Network),
    (libc::SYS_getsockname, Unobservable::Network),
    (libc::SYS_getpeername, Unobservable::Network),
    (libc::SYS_shutdown, Unobservable::Network),
    #[cfg(not(target_arch = "riscv64"))]
    (libc::SYS_renameat, Unobservable::FileWrite),
    (libc::SYS_unlinkat, Unobservable::FileWrite),
    (libc::SYS_copy_file_range, Unobservable::FileWrite),
    (libc::SYS_renameat2, Unobservable::FileWrite),
    (libc::SYS_mkdirat, Unobservable::FileWrite),
    (libc::SYS_mknodat, Unobservable::FileWrite),
    (libc::SYS_linkat, Unobservable::FileWrite),
    (libc::SYS_symlinkat, Unobservable::FileWrite),
    (libc::SYS_fchmodat, Unobservable::FileWrite),
    (libc::SYS_fchownat, Unobservable::FileWrite),
    (libc::SYS_utimensat, Unobservable::FileWrite),
    (libc::SYS_truncate, Unobservable::FileWrite),
    (libc::SYS_ftruncate, Unobservable::FileWrite),
    (libc::SYS_fallocate, Unobservable::FileWrite),
    (libc::SYS_memfd_create, Unobservable::FileWrite),
    (libc::SYS_fchmod, Unobservable::FileWrite),
    (libc::SYS_fchown, Unobservable::FileWrite),
    (libc::SYS_uname, Unobservable::HostState),
    (libc::SYS_sysinfo, Unobservable::HostState),
    (libc::SYS_statfs, Unobservable::HostState),
    (libc::SYS_fstatfs, Unobservable::HostState),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_fork, Unobservable::ProcessControl),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_vfork, Unobservable::ProcessControl),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_creat, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_unlink, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_rename, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_mkdir, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_rmdir, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_link, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_symlink, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_chmod, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_chown, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_lchown, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_utime, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_utimes, Unobservable::FileWrite),
    #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
    (libc::SYS_mknod, Unobservable::FileWrite),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::macro_observation::hooks::tests::{ImportClass, import_class};

    /// The GNU C library functions that perform each classified system call's operation.
    const WRAPPERS: &[(c_long, &[&str])] = &[
        (libc::SYS_read, &["eventfd_read", "read"]),
        (libc::SYS_write, &["eventfd_write", "write"]),
        (libc::SYS_readv, &["readv"]),
        (libc::SYS_writev, &["writev"]),
        (libc::SYS_pread64, &["pread", "pread64"]),
        (libc::SYS_pwrite64, &["pwrite", "pwrite64"]),
        (libc::SYS_preadv, &["preadv", "preadv64"]),
        (libc::SYS_pwritev, &["pwritev", "pwritev64"]),
        (libc::SYS_close, &["close"]),
        (libc::SYS_fstat, &["__fxstat", "__fxstat64", "fstat", "fstat64"]),
        (libc::SYS_lseek, &["lseek", "lseek64"]),
        (libc::SYS_munmap, &["munmap"]),
        (libc::SYS_mremap, &["mremap"]),
        (libc::SYS_madvise, &["madvise"]),
        (libc::SYS_brk, &[]),
        (libc::SYS_rt_sigaction, &["sigaction", "signal"]),
        (libc::SYS_rt_sigprocmask, &["pthread_sigmask", "sigprocmask"]),
        (libc::SYS_rt_sigreturn, &[]),
        (libc::SYS_sigaltstack, &["sigaltstack"]),
        (libc::SYS_futex, &[]),
        (libc::SYS_sched_yield, &["sched_yield"]),
        (libc::SYS_sched_getaffinity, &["sched_getaffinity"]),
        (libc::SYS_nanosleep, &["nanosleep", "sleep", "usleep"]),
        (libc::SYS_clock_nanosleep, &["clock_nanosleep"]),
        (libc::SYS_clock_gettime, &["clock_gettime", "time"]),
        (libc::SYS_clock_getres, &["clock_getres"]),
        (libc::SYS_gettimeofday, &["gettimeofday"]),
        (libc::SYS_getpid, &["getpid"]),
        (libc::SYS_gettid, &["gettid"]),
        (libc::SYS_getppid, &["getppid"]),
        (libc::SYS_getuid, &["getuid"]),
        (libc::SYS_geteuid, &["geteuid"]),
        (libc::SYS_getgid, &["getgid"]),
        (libc::SYS_getegid, &["getegid"]),
        (libc::SYS_exit, &[]),
        (libc::SYS_exit_group, &["_Exit", "_exit"]),
        (libc::SYS_set_robust_list, &[]),
        (libc::SYS_get_robust_list, &[]),
        (libc::SYS_rseq, &[]),
        (libc::SYS_set_tid_address, &[]),
        (libc::SYS_getrandom, &["getentropy", "getrandom"]),
        (libc::SYS_membarrier, &[]),
        (libc::SYS_prlimit64, &["getrlimit", "getrlimit64"]),
        (libc::SYS_dup, &["dup"]),
        (libc::SYS_dup3, &["dup2", "dup3"]),
        (libc::SYS_pipe2, &["pipe", "pipe2"]),
        (libc::SYS_fcntl, &["fcntl"]),
        (libc::SYS_ioctl, &["ioctl", "isatty"]),
        (libc::SYS_ppoll, &["poll", "ppoll"]),
        (libc::SYS_pselect6, &["pselect", "select"]),
        (libc::SYS_wait4, &["wait", "wait4", "waitpid"]),
        (libc::SYS_waitid, &["waitid"]),
        (libc::SYS_kill, &["kill", "killpg"]),
        (libc::SYS_tgkill, &["pthread_kill", "raise", "tgkill"]),
        (libc::SYS_tkill, &[]),
        (libc::SYS_restart_syscall, &[]),
        (libc::SYS_getcwd, &["getcwd"]),
        (libc::SYS_getrusage, &["getrusage"]),
        (libc::SYS_fsync, &["fsync"]),
        (libc::SYS_fdatasync, &["fdatasync"]),
        (libc::SYS_flock, &["flock"]),
        (libc::SYS_sendfile, &["sendfile", "sendfile64"]),
        (libc::SYS_splice, &["splice"]),
        (libc::SYS_pidfd_open, &["pidfd_open"]),
        (libc::SYS_pidfd_send_signal, &["pidfd_send_signal"]),
        (libc::SYS_eventfd2, &["eventfd"]),
        (libc::SYS_epoll_create1, &["epoll_create", "epoll_create1"]),
        (libc::SYS_epoll_ctl, &["epoll_ctl"]),
        (libc::SYS_epoll_pwait, &["epoll_pwait", "epoll_wait"]),
        (libc::SYS_epoll_pwait2, &["epoll_pwait2"]),
        #[cfg(target_arch = "x86_64")]
        (libc::SYS_arch_prctl, &[]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_poll, &["poll"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_dup2, &["dup2"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_pipe, &["pipe"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_pause, &["pause"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_eventfd, &[]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_epoll_create, &["epoll_create"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_epoll_wait, &["epoll_wait"]),
        (libc::SYS_chdir, &["chdir"]),
        (libc::SYS_fchdir, &["fchdir"]),
        (libc::SYS_chroot, &["chroot"]),
        (libc::SYS_execve, &["execv", "execve", "execvp", "execvpe"]),
        (libc::SYS_execveat, &["fexecve"]),
        (libc::SYS_clone3, &[]),
        (libc::SYS_setuid, &["setuid"]),
        (libc::SYS_setgid, &["setgid"]),
        (libc::SYS_setgroups, &["setgroups"]),
        (libc::SYS_setpgid, &["setpgid"]),
        (libc::SYS_setsid, &["setsid"]),
        (libc::SYS_socket, &["socket"]),
        (libc::SYS_socketpair, &["socketpair"]),
        (libc::SYS_connect, &["connect"]),
        (libc::SYS_bind, &["bind"]),
        (libc::SYS_listen, &["listen"]),
        #[cfg(not(target_arch = "s390x"))]
        (libc::SYS_accept, &["accept"]),
        (libc::SYS_accept4, &["accept", "accept4"]),
        (libc::SYS_sendto, &["send", "sendto"]),
        (libc::SYS_sendmsg, &["sendmsg"]),
        (libc::SYS_sendmmsg, &[]),
        (libc::SYS_recvfrom, &["recv", "recvfrom"]),
        (libc::SYS_recvmsg, &["recvmsg"]),
        (libc::SYS_recvmmsg, &[]),
        (libc::SYS_getsockopt, &["getsockopt"]),
        (libc::SYS_setsockopt, &["setsockopt"]),
        (libc::SYS_getsockname, &["getsockname"]),
        (libc::SYS_getpeername, &["getpeername"]),
        (libc::SYS_shutdown, &["shutdown"]),
        #[cfg(not(target_arch = "riscv64"))]
        (libc::SYS_renameat, &["renameat"]),
        (libc::SYS_unlinkat, &["unlinkat"]),
        (libc::SYS_copy_file_range, &["copy_file_range"]),
        (libc::SYS_renameat2, &["renameat2"]),
        (libc::SYS_mkdirat, &["mkdirat"]),
        (libc::SYS_mknodat, &["mkfifoat", "mknodat"]),
        (libc::SYS_linkat, &["linkat"]),
        (libc::SYS_symlinkat, &["symlinkat"]),
        (libc::SYS_fchmodat, &["fchmodat"]),
        (libc::SYS_fchownat, &["fchownat"]),
        (libc::SYS_utimensat, &["futimens", "futimesat", "lutimes", "utimensat"]),
        (libc::SYS_truncate, &["truncate", "truncate64"]),
        (libc::SYS_ftruncate, &["ftruncate", "ftruncate64"]),
        (
            libc::SYS_fallocate,
            &["fallocate", "fallocate64", "posix_fallocate", "posix_fallocate64"],
        ),
        (libc::SYS_memfd_create, &["memfd_create"]),
        (libc::SYS_fchmod, &["fchmod"]),
        (libc::SYS_fchown, &["fchown"]),
        (libc::SYS_uname, &["uname"]),
        (libc::SYS_sysinfo, &["sysinfo"]),
        (libc::SYS_statfs, &["statfs", "statfs64", "statvfs", "statvfs64"]),
        (libc::SYS_fstatfs, &["fstatfs", "fstatfs64", "fstatvfs", "fstatvfs64"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_fork, &["fork"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_vfork, &["vfork"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_creat, &["creat", "creat64"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_unlink, &["unlink"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_rename, &["rename"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_mkdir, &["mkdir"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_rmdir, &["rmdir"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_link, &["link"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_symlink, &["symlink"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_chmod, &["chmod"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_chown, &["chown"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_lchown, &["lchown"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_utime, &["utime"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_utimes, &["utimes"]),
        #[cfg(any(target_arch = "x86_64", target_arch = "s390x", target_arch = "powerpc64"))]
        (libc::SYS_mknod, &["mkfifo", "mknod"]),
    ];

    /// System calls whose arguments decide what they read or which effect they have, and the functions
    /// that perform them. Each function needs a hook; a pure classification would skip the observation.
    const OBSERVED: &[(c_long, &[&str])] = &[
        (
            libc::SYS_openat,
            &[
                "__open64_2",
                "__open_2",
                "__openat64_2",
                "__openat_2",
                "fopen",
                "fopen64",
                "freopen",
                "freopen64",
                "open",
                "open64",
                "openat",
                "openat64",
            ],
        ),
        (
            libc::SYS_newfstatat,
            &[
                "__fxstatat",
                "__fxstatat64",
                "__lxstat",
                "__lxstat64",
                "__xstat",
                "__xstat64",
                "fstatat",
                "fstatat64",
                "lstat",
                "lstat64",
                "stat",
                "stat64",
            ],
        ),
        (libc::SYS_statx, &["statx"]),
        (libc::SYS_faccessat, &["access", "eaccess", "euidaccess", "faccessat"]),
        (libc::SYS_readlinkat, &["readlink", "readlinkat"]),
        (libc::SYS_getdents64, &["getdents64"]),
        (libc::SYS_mmap, &["mmap", "mmap64"]),
        (libc::SYS_mprotect, &["mprotect"]),
        #[cfg(not(any(target_arch = "s390x", target_arch = "powerpc64")))]
        (libc::SYS_pkey_mprotect, &["pkey_mprotect"]),
        (libc::SYS_clone, &["_Fork", "clone", "fork", "vfork"]),
    ];

    fn system_call_class(number: c_long) -> Option<ImportClass> {
        if PURE_SYSTEM_CALLS.contains(&number) {
            return Some(ImportClass::Pure);
        }
        EFFECT_SYSTEM_CALLS
            .iter()
            .find(|(effect, _)| *effect == number)
            .map(|(_, effect)| ImportClass::Effect(*effect))
    }

    #[test]
    fn import_and_system_call_tables_classify_each_operation_alike() {
        let effects = EFFECT_SYSTEM_CALLS
            .iter()
            .map(|(number, _)| *number)
            .collect::<Vec<_>>();
        let mapped = WRAPPERS.iter().map(|(number, _)| *number).collect::<BTreeSet<_>>();
        assert_eq!(mapped.len(), WRAPPERS.len(), "a system call is mapped twice");
        for number in PURE_SYSTEM_CALLS.iter().chain(&effects) {
            assert!(
                !PURE_SYSTEM_CALLS.contains(number) || !effects.contains(number),
                "system call {number} is both pure and an effect"
            );
            assert!(
                mapped.contains(number),
                "system call {number} names no C library functions"
            );
        }
        for &(number, wrappers) in WRAPPERS {
            let expected = system_call_class(number)
                .unwrap_or_else(|| panic!("system call {number} names functions but has no classification"));
            for wrapper in wrappers {
                assert_eq!(
                    import_class(wrapper),
                    expected,
                    "{wrapper} performs system call {number}"
                );
            }
        }
        for &(number, wrappers) in OBSERVED {
            assert_eq!(system_call_class(number), None, "system call {number} is observed");
            for wrapper in wrappers {
                assert!(
                    matches!(import_class(wrapper), ImportClass::Observed | ImportClass::Effect(_)),
                    "{wrapper} performs observed system call {number} without a hook"
                );
            }
        }
    }
}
