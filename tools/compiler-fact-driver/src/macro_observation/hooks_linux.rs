//! GNU C library symbol variants and Linux-only entry points.

use std::ffi::c_long;

use super::*;

hooks! {
    forwarding {
        open_2 = "__open_2" => |a| observe_open(libc::AT_FDCWD, path(a[0]), a[1] as c_int);
        open64_2 = "__open64_2" => |a| observe_open(libc::AT_FDCWD, path(a[0]), a[1] as c_int);
        openat_2 = "__openat_2" => |a| observe_open(descriptor(a[0]), path(a[1]), a[2] as c_int);
        openat64_2 = "__openat64_2" => |a| observe_open(descriptor(a[0]), path(a[1]), a[2] as c_int);
        creat = "creat" => |_a| record_unobservable(Unobservable::FileWrite);
        creat64 = "creat64" => |_a| record_unobservable(Unobservable::FileWrite);
        fopen64 = "fopen64" => |a| observe_fopen(path(a[0]), path(a[1]));
        freopen64 = "freopen64" => |a| observe_fopen(path(a[0]), path(a[1]));
        stat64 = "stat64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        lstat64 = "lstat64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        fstatat64 = "fstatat64" => |a| observe_entry_at(descriptor(a[0]), path(a[1]), a[3] as c_int);
        xstat = "__xstat" => |a| observe(libc::AT_FDCWD, path(a[1]), Access::Entry);
        xstat64 = "__xstat64" => |a| observe(libc::AT_FDCWD, path(a[1]), Access::Entry);
        lxstat = "__lxstat" => |a| observe(libc::AT_FDCWD, path(a[1]), Access::Entry);
        lxstat64 = "__lxstat64" => |a| observe(libc::AT_FDCWD, path(a[1]), Access::Entry);
        fxstatat = "__fxstatat" => |a| observe_entry_at(descriptor(a[1]), path(a[2]), a[4] as c_int);
        fxstatat64 = "__fxstatat64" => |a| observe_entry_at(descriptor(a[1]), path(a[2]), a[4] as c_int);
        statx = "statx" => |a| observe_entry_at(descriptor(a[0]), path(a[1]), a[2] as c_int);
        euidaccess = "euidaccess" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        eaccess = "eaccess" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        realpath_chk = "__realpath_chk" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Resolution);
        canonicalize_file_name = "canonicalize_file_name"
            => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Resolution);
        scandir64 = "scandir64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        scandirat = "scandirat" => |a| observe(descriptor(a[0]), path(a[1]), Access::Listing);
        getdents64 = "getdents64" => |a| observe_listing_descriptor(descriptor(a[0]));
        secure_getenv = "secure_getenv" => |a| observe_environment(a[0]);
        secure_getenv_internal = "__secure_getenv" => |a| observe_environment(a[0]);
        clearenv = "clearenv" => |_a| record_unobservable(Unobservable::EnvironmentWrite);
        fork_internal = "_Fork" => |_a| record_unobservable(Unobservable::ProcessControl);
        clone = "clone" => |_a| record_unobservable(Unobservable::ProcessControl);
        execvpe = "execvpe" => |_a| record_unobservable(Unobservable::ProcessControl);
        pidfd_spawn = "pidfd_spawn" => |_a| record_unobservable(Unobservable::ProcessControl);
        pidfd_spawnp = "pidfd_spawnp" => |_a| record_unobservable(Unobservable::ProcessControl);
        dlmopen = "dlmopen" => |_a| record_unobservable(Unobservable::DynamicLoad);
        mmap64 = "mmap64" => |a| observe_executable_memory(a[2]);
        pkey_mprotect = "pkey_mprotect" => |a| observe_executable_memory(a[2]);
        memfd_create = "memfd_create" => |_a| record_unobservable(Unobservable::FileWrite);
        renameat2 = "renameat2" => |_a| record_unobservable(Unobservable::FileWrite);
        truncate64 = "truncate64" => |_a| record_unobservable(Unobservable::FileWrite);
        ftruncate64 = "ftruncate64" => |_a| record_unobservable(Unobservable::FileWrite);
        utime = "utime" => |_a| record_unobservable(Unobservable::FileWrite);
        lutimes = "lutimes" => |_a| record_unobservable(Unobservable::FileWrite);
        futimesat = "futimesat" => |_a| record_unobservable(Unobservable::FileWrite);
        mkstemp64 = "mkstemp64" => |_a| record_unobservable(Unobservable::FileWrite);
        mkostemp64 = "mkostemp64" => |_a| record_unobservable(Unobservable::FileWrite);
        mkostemps = "mkostemps" => |_a| record_unobservable(Unobservable::FileWrite);
        tmpfile64 = "tmpfile64" => |_a| record_unobservable(Unobservable::FileWrite);
        mkfifoat = "mkfifoat" => |_a| record_unobservable(Unobservable::FileWrite);
        mknodat = "mknodat" => |_a| record_unobservable(Unobservable::FileWrite);
        fallocate = "fallocate" => |_a| record_unobservable(Unobservable::FileWrite);
        fallocate64 = "fallocate64" => |_a| record_unobservable(Unobservable::FileWrite);
        posix_fallocate = "posix_fallocate" => |_a| record_unobservable(Unobservable::FileWrite);
        posix_fallocate64 = "posix_fallocate64" => |_a| record_unobservable(Unobservable::FileWrite);
        gethostbyname_r = "gethostbyname_r" => |_a| record_unobservable(Unobservable::Network);
        res_init = "res_init" => |_a| record_unobservable(Unobservable::Network);
        res_init_internal = "__res_init" => |_a| record_unobservable(Unobservable::Network);
        gethostbyname2_r = "gethostbyname2_r" => |_a| record_unobservable(Unobservable::Network);
        sysinfo = "sysinfo" => |_a| record_unobservable(Unobservable::HostState);
        statvfs64 = "statvfs64" => |_a| record_unobservable(Unobservable::HostState);
        fstatvfs64 = "fstatvfs64" => |_a| record_unobservable(Unobservable::HostState);
        statfs64 = "statfs64" => |_a| record_unobservable(Unobservable::HostState);
        fstatfs64 = "fstatfs64" => |_a| record_unobservable(Unobservable::HostState);
    }
    explicit {
        open64 = "open64";
        openat64 = "openat64";
        syscall = "syscall";
        environ = "environ";
        environ_internal = "__environ";
    }
}

mod explicit {
    use super::*;

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn open64(path: *const c_char, flags: c_int, mut arguments: ...) -> c_int {
        // SAFETY: the C library reads the mode argument exactly when the flags ask for it.
        let mode = if super::super::explicit::takes_mode(flags) {
            unsafe { arguments.next_arg::<c_int>() }
        } else {
            0
        };
        if observed_process() {
            observe_open(libc::AT_FDCWD, path, flags);
        }
        // SAFETY: rebind installed this hook only in a slot that held the original.
        unsafe {
            let function: unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int =
                std::mem::transmute(original(&originals::open64));
            function(path, flags, mode)
        }
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn openat64(
        directory: c_int,
        path: *const c_char,
        flags: c_int,
        mut arguments: ...
    ) -> c_int {
        // SAFETY: as for `open64`.
        let mode = if super::super::explicit::takes_mode(flags) {
            unsafe { arguments.next_arg::<c_int>() }
        } else {
            0
        };
        if observed_process() {
            observe_open(directory, path, flags);
        }
        // SAFETY: rebind installed this hook only in a slot that held the original.
        unsafe {
            let function: unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int =
                std::mem::transmute(original(&originals::openat64));
            function(directory, path, flags, mode)
        }
    }

    /// Observe a system call made through the C library's generic entry point, then make it.
    ///
    /// Every Linux system call takes at most six integer arguments, so reading six is exact.
    ///
    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn syscall(number: c_long, mut arguments: ...) -> c_long {
        // SAFETY: the caller passes the arguments its system call takes; reading more returns unused values.
        let a = unsafe {
            [
                arguments.next_arg::<c_long>(),
                arguments.next_arg::<c_long>(),
                arguments.next_arg::<c_long>(),
                arguments.next_arg::<c_long>(),
                arguments.next_arg::<c_long>(),
                arguments.next_arg::<c_long>(),
            ]
        };
        if observed_process() {
            super::super::super::seccomp::observe_system_call(number, a.map(|argument| argument as u64));
        }
        // SAFETY: rebind installed this hook only in a slot that held the original.
        unsafe {
            let function: unsafe extern "C" fn(c_long, ...) -> c_long =
                std::mem::transmute(original(&originals::syscall));
            function(number, a[0], a[1], a[2], a[3], a[4], a[5])
        }
    }

    // `environ` and `__environ` are data. Their hooks are never called; `hooks` replaces their addresses
    // with the guarded environment variable.
    /// # Safety
    ///
    /// Never called; the slot holds the guarded environment variable instead.
    pub(super) unsafe extern "C" fn environ() {}
    /// # Safety
    ///
    /// Never called; the slot holds the guarded environment variable instead.
    pub(super) unsafe extern "C" fn environ_internal() {}
}

pub(in super::super) fn platform_hooks() -> &'static [Hook] {
    hooks()
}

/// GNU C library functions with no effect a result could depend on. Keep the list sorted.
pub(in super::super) const PLATFORM_PURE: &[&str] = &[
    "_ITM_deregisterTMCloneTable",
    "_ITM_registerTMCloneTable",
    "__cxa_atexit",
    "__cxa_finalize",
    "__cxa_thread_atexit_impl",
    "__errno_location",
    "__fxstat",
    "__fxstat64",
    "__gmon_start__",
    "__libc_single_threaded",
    "__memcpy_chk",
    "__memmove_chk",
    "__memset_chk",
    "__pthread_get_minstack",
    "__stack_chk_fail",
    "__tls_get_addr",
    "__xpg_strerror_r",
    "dl_iterate_phdr",
    "fstat64",
    "getauxval",
    "getrandom",
    "gettid",
    "gnu_get_libc_version",
    "lseek64",
    "mremap",
    "pidfd_getpid",
    "pidfd_open",
    "pidfd_send_signal",
    "pipe2",
    "pread64",
    "pthread_getattr_np",
    "pthread_getname_np",
    "pwrite",
    "pwrite64",
    "readdir64",
    "readdir64_r",
    "sched_getaffinity",
];
