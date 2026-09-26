//! Apple symbol variants and Apple-only entry points.

use super::*;

hooks! {
    forwarding {
        stat_inode64 = "stat$INODE64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        lstat_inode64 = "lstat$INODE64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        fstatat_inode64 = "fstatat$INODE64" => |a| observe_entry_at(descriptor(a[0]), path(a[1]), a[3] as c_int);
        getattrlist = "getattrlist" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        getattrlistat = "getattrlistat" => |a| observe(descriptor(a[0]), path(a[1]), Access::Entry);
        opendir_inode64 = "opendir$INODE64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        fdopendir_inode64 = "fdopendir$INODE64" => |a| observe_listing_descriptor(descriptor(a[0]));
        opendir2 = "__opendir2" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        opendir2_inode64 = "__opendir2$INODE64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        scandir_inode64 = "scandir$INODE64" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        getattrlistbulk = "getattrlistbulk" => |a| observe_listing_descriptor(descriptor(a[0]));
        getdirentries = "getdirentries" => |a| observe_listing_descriptor(descriptor(a[0]));
        getdirentriesattr = "getdirentriesattr" => |a| observe_listing_descriptor(descriptor(a[0]));
        realpath_darwin = "realpath$DARWIN_EXTSN" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Resolution);
        fopen_darwin = "fopen$DARWIN_EXTSN" => |a| observe_fopen(path(a[0]), path(a[1]));
        renamex_np = "renamex_np" => |_a| record_unobservable(Unobservable::FileWrite);
        renameatx_np = "renameatx_np" => |_a| record_unobservable(Unobservable::FileWrite);
        clonefile = "clonefile" => |_a| record_unobservable(Unobservable::FileWrite);
        clonefileat = "clonefileat" => |_a| record_unobservable(Unobservable::FileWrite);
        fclonefileat = "fclonefileat" => |_a| record_unobservable(Unobservable::FileWrite);
        copyfile = "copyfile" => |_a| record_unobservable(Unobservable::FileWrite);
        fcopyfile = "fcopyfile" => |_a| record_unobservable(Unobservable::FileWrite);
        lchmod = "lchmod" => |_a| record_unobservable(Unobservable::FileWrite);
        dlopen_preflight = "dlopen_preflight" => |_a| record_unobservable(Unobservable::DynamicLoad);
        jit_write_protect = "pthread_jit_write_protect_np"
            => |_a| record_unobservable(Unobservable::ExecutableMemory);
        sysctl = "sysctl" => |_a| record_unobservable(Unobservable::HostState);
        sysctlbyname = "sysctlbyname" => |_a| record_unobservable(Unobservable::HostState);
        sysctlnametomib = "sysctlnametomib" => |_a| record_unobservable(Unobservable::HostState);
        statfs_inode64 = "statfs$INODE64" => |_a| record_unobservable(Unobservable::HostState);
        fstatfs_inode64 = "fstatfs$INODE64" => |_a| record_unobservable(Unobservable::HostState);
        getmntinfo = "getmntinfo" => |_a| record_unobservable(Unobservable::HostState);
        getfsstat = "getfsstat" => |_a| record_unobservable(Unobservable::HostState);
        executable_path = "_NSGetExecutablePath" => |_a| record_unobservable(Unobservable::HostState);
    }
    explicit {
        ns_get_environ = "_NSGetEnviron";
        open_nocancel = "open$NOCANCEL";
        openat_nocancel = "openat$NOCANCEL";
    }
}

mod explicit {
    use super::*;

    /// The macro receives the address of its guarded environment variable, never the process environment.
    ///
    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn ns_get_environ() -> *mut *mut *mut c_char {
        guarded_environment_variable() as *mut *mut *mut c_char
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn open_nocancel(path: *const c_char, flags: c_int, mut arguments: ...) -> c_int {
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
                std::mem::transmute(original(&originals::open_nocancel));
            function(path, flags, mode)
        }
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn openat_nocancel(
        directory: c_int,
        path: *const c_char,
        flags: c_int,
        mut arguments: ...
    ) -> c_int {
        // SAFETY: as for `open_nocancel`.
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
                std::mem::transmute(original(&originals::openat_nocancel));
            function(directory, path, flags, mode)
        }
    }
}

pub(in super::super) fn platform_hooks() -> &'static [Hook] {
    hooks()
}

/// Apple-only functions with no effect a result could depend on. Keep the list sorted.
pub(in super::super) const PLATFORM_PURE: &[&str] = &[
    "CCRandomGenerateBytes",
    "_NSGetArgc",
    "_NSGetArgv",
    "__error",
    "__stack_chk_fail",
    "__stack_chk_guard",
    "_dyld_get_image_header",
    "_dyld_get_image_name",
    "_dyld_get_image_vmaddr_slide",
    "_dyld_image_count",
    "_tlv_atexit",
    "_tlv_bootstrap",
    "arc4random",
    "arc4random_buf",
    "arc4random_uniform",
    "dispatch_release",
    "dispatch_semaphore_create",
    "dispatch_semaphore_signal",
    "dispatch_semaphore_wait",
    "dyld_stub_binder",
    "fstat$INODE64",
    "mach_absolute_time",
    "mach_timebase_info",
    "os_unfair_lock_lock",
    "os_unfair_lock_trylock",
    "os_unfair_lock_unlock",
    "pthread_get_stackaddr_np",
    "pthread_get_stacksize_np",
    "pthread_threadid_np",
    "readdir$INODE64",
    "readdir_r$INODE64",
];
