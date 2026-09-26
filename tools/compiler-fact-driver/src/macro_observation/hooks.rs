//! Import classification and the hooks that record what a macro image reaches through the C library.
//!
//! Each hook records the access and then calls the original function with the same arguments, so the macro
//! behaves exactly as without observation. Hooks record only in the compiler process: a forked child
//! inherits the rebound slots but must not take the recorder lock, and the fork itself is already reported.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::{
    ENVIRONMENT_GUARD, Unobservable, absolute_path, argument_vector, c_string, c_string_bytes, descriptor_path,
    page_size, record_environment, record_path, record_spawn, record_unobservable, record_unobservable_import,
};
use crate::native_input_protocol::NativeMacroPathAccess as Access;

/// How instrumentation treats one imported symbol.
pub(super) enum Class {
    /// No filesystem, environment, process, or network effect that the result could depend on.
    Pure,
    Hooked(&'static Hook),
    Unclassified,
}

pub(super) struct Hook {
    symbol: &'static str,
    function: usize,
    original: &'static AtomicUsize,
}

static PROCESS: AtomicU32 = AtomicU32::new(0);

/// Whether the caller runs in the observed compiler process rather than a forked child.
fn observed_process() -> bool {
    PROCESS.load(Ordering::Relaxed) == std::process::id()
}

fn all_hooks() -> impl Iterator<Item = &'static Hook> {
    hooks().iter().chain(platform::platform_hooks())
}

fn original(hook: &AtomicUsize) -> usize {
    hook.load(Ordering::Acquire)
}

/// Addresses of every hooked function, which a macro image may reference only through a rebound slot.
#[cfg(target_os = "macos")]
pub(super) fn hooked_originals() -> std::collections::BTreeSet<usize> {
    all_hooks()
        .map(|hook| original(hook.original))
        .filter(|address| *address != 0)
        .collect()
}

/// Resolve every hooked symbol through the default search order before any macro loads.
pub(super) fn resolve_originals() {
    PROCESS.store(std::process::id(), Ordering::Relaxed);
    for hook in all_hooks() {
        let Ok(name) = std::ffi::CString::new(hook.symbol) else {
            continue;
        };
        // SAFETY: dlsym only reads the NUL-terminated name.
        let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        hook.original.store(address as usize, Ordering::Release);
    }
}

pub(super) fn classify(name: &str) -> Class {
    if let Some(hook) = all_hooks().find(|hook| hook.symbol == name) {
        return Class::Hooked(hook);
    }
    if PURE.binary_search(&name).is_ok() || PLATFORM_PURE.binary_search(&name).is_ok() {
        Class::Pure
    } else {
        Class::Unclassified
    }
}

impl Hook {
    /// What a rebound slot holds: the hook, or for the environment variable the guarded copy.
    fn replacement(&self) -> usize {
        if matches!(self.symbol, "environ" | "__environ") {
            guarded_environment_variable()
        } else {
            self.function
        }
    }
}

/// Point one import slot at its hook. The slot must hold the function that the default search order
/// resolves, so the hook forwards to exactly the function the macro would have called.
pub(super) fn rebind(slot: *mut usize, hook: &Hook) -> Result<(), ()> {
    rebind_slot(slot, original(hook.original), hook.replacement())
}

/// Replace `original` with `replacement` in one loaded image's import slot, preserving page protection.
pub(super) fn rebind_slot(slot: *mut usize, original: usize, replacement: usize) -> Result<(), ()> {
    // SAFETY: the object-format parser derived the slot from the loaded image's own import table.
    let bound = unsafe { slot.read_volatile() };
    if bound == 0 && original == 0 {
        // A weak import that no loaded library defines. The image checks for null before calling it.
        return Ok(());
    }
    if original == 0 || bound != original && bound != replacement {
        return Err(());
    }
    let page = page_size();
    let start = (slot as usize) & !(page - 1);
    let end = (slot as usize + size_of::<usize>()).div_ceil(page) * page;
    let protection = region_protection(start).ok_or(())?;
    // SAFETY: the range covers only the image's own mapped import pages; the original protection is
    // restored before the image runs.
    unsafe {
        if protection & libc::PROT_WRITE == 0
            && libc::mprotect(start as *mut c_void, end - start, protection | libc::PROT_WRITE) != 0
        {
            return Err(());
        }
        slot.write_volatile(replacement);
        if protection & libc::PROT_WRITE == 0 && libc::mprotect(start as *mut c_void, end - start, protection) != 0 {
            return Err(());
        }
    }
    Ok(())
}

/// The current protection of the page that holds `address`.
fn region_protection(address: usize) -> Option<c_int> {
    #[cfg(target_os = "linux")]
    {
        let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
        maps.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            let (start, end) = fields.next()?.split_once('-')?;
            let start = usize::from_str_radix(start, 16).ok()?;
            let end = usize::from_str_radix(end, 16).ok()?;
            if address < start || address >= end {
                return None;
            }
            let permissions = fields.next()?.as_bytes();
            let mut protection = 0;
            if permissions.first() == Some(&b'r') {
                protection |= libc::PROT_READ;
            }
            if permissions.get(1) == Some(&b'w') {
                protection |= libc::PROT_WRITE;
            }
            if permissions.get(2) == Some(&b'x') {
                protection |= libc::PROT_EXEC;
            }
            Some(protection)
        })
    }
    #[cfg(target_os = "macos")]
    {
        super::macho::region_protection(address)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = address;
        None
    }
}

// Every forwarding hook takes six integer registers and returns one. Each function it replaces takes at
// most six integer or pointer arguments, none of them variadic or floating point, and returns an integer or
// a pointer, so reading unused argument registers is harmless and every used argument reaches the original
// in its own register or stack slot. Variadic functions have explicit hooks below.
type Forwarder = unsafe extern "C" fn(usize, usize, usize, usize, usize, usize) -> usize;

macro_rules! hooks {
    (
        forwarding { $($forward:ident = $forward_symbol:literal => |$arguments:ident| $observe:expr;)* }
        explicit { $($explicit:ident = $explicit_symbol:literal;)* }
    ) => {
        mod originals {
            use std::sync::atomic::AtomicUsize;
            $(#[allow(non_upper_case_globals)] pub(super) static $forward: AtomicUsize = AtomicUsize::new(0);)*
            $(#[allow(non_upper_case_globals)] pub(super) static $explicit: AtomicUsize = AtomicUsize::new(0);)*
        }

        mod forwarding {
            use super::*;
            $(
                #[allow(non_snake_case)]
                /// # Safety
                ///
                /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
                pub(super) unsafe extern "C" fn $forward(
                    a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize,
                ) -> usize {
                    if observed_process() {
                        let $arguments = [a0, a1, a2, a3, a4, a5];
                        $observe;
                    }
                    // SAFETY: resolve_originals stored this symbol's address, and rebind only installs the
                    // hook in a slot that held it.
                    let function: Forwarder = unsafe { std::mem::transmute(original(&originals::$forward)) };
                    unsafe { function(a0, a1, a2, a3, a4, a5) }
                }
            )*
        }

        fn hooks() -> &'static [Hook] {
            static HOOKS: OnceLock<Vec<Hook>> = OnceLock::new();
            HOOKS.get_or_init(|| vec![
                $(Hook {
                    symbol: $forward_symbol,
                    function: forwarding::$forward as *const () as usize,
                    original: &originals::$forward,
                },)*
                $(Hook {
                    symbol: $explicit_symbol,
                    function: explicit::$explicit as *const () as usize,
                    original: &originals::$explicit,
                },)*
            ])
        }
    };
}

fn path(argument: usize) -> *const c_char {
    argument as *const c_char
}

fn descriptor(argument: usize) -> c_int {
    argument as c_int
}

/// Record a read of `path`, resolved against `directory`.
fn observe(directory: c_int, path: *const c_char, access: Access) {
    // SAFETY: the macro passes the path to the C library, which requires a valid C string or null.
    if let Some(path) = unsafe { absolute_path(directory, path) } {
        record_path(path, access);
    }
}

/// `flags` may name `AT_EMPTY_PATH`, which makes an empty path refer to the descriptor itself.
fn observe_entry_at(directory: c_int, path: *const c_char, flags: c_int) {
    #[cfg(target_os = "linux")]
    if flags & libc::AT_EMPTY_PATH != 0 && c_string_bytes(path).is_some_and(|bytes| bytes.is_empty()) {
        // Metadata of an already open descriptor, whose path was observed when it was opened.
        return;
    }
    let _ = flags;
    observe(directory, path, Access::Entry);
}

fn observe_open(directory: c_int, path: *const c_char, flags: c_int) {
    #[cfg(target_os = "linux")]
    let anonymous = flags & libc::O_TMPFILE == libc::O_TMPFILE;
    #[cfg(not(target_os = "linux"))]
    let anonymous = false;
    let writes = anonymous || flags & libc::O_ACCMODE != libc::O_RDONLY || flags & (libc::O_CREAT | libc::O_TRUNC) != 0;
    #[cfg(target_os = "linux")]
    if flags & libc::O_PATH != 0 && !writes {
        observe(directory, path, Access::Entry);
        return;
    }
    if writes {
        record_unobservable(Unobservable::FileWrite);
    } else if flags & libc::O_DIRECTORY != 0 {
        observe(directory, path, Access::Entry);
    } else {
        observe(directory, path, Access::Contents);
    }
}

fn observe_fopen(path: *const c_char, mode: *const c_char) {
    let reads_only = c_string_bytes(mode).is_some_and(|mode| mode.first() == Some(&b'r') && !mode.contains(&b'+'));
    if reads_only {
        observe(libc::AT_FDCWD, path, Access::Contents);
    } else {
        record_unobservable(Unobservable::FileWrite);
    }
}

fn observe_listing_descriptor(descriptor: c_int) {
    match descriptor_path(descriptor) {
        Some(path) => record_path(path, Access::Listing),
        None => record_unobservable(Unobservable::PathUnavailable),
    }
}

fn observe_executable_memory(protection: usize) {
    if protection as c_int & libc::PROT_EXEC != 0 {
        record_unobservable(Unobservable::ExecutableMemory);
    }
}

fn observe_environment(name: usize) {
    if let Some(name) = c_string_bytes(name as *const c_char) {
        record_environment(&name);
    }
}

hooks! {
    forwarding {
        // Metadata, existence, and link targets.
        stat = "stat" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        lstat = "lstat" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        fstatat = "fstatat" => |a| observe_entry_at(descriptor(a[0]), path(a[1]), a[3] as c_int);
        access = "access" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        faccessat = "faccessat" => |a| observe_entry_at(descriptor(a[0]), path(a[1]), a[3] as c_int);
        readlink = "readlink" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        readlinkat = "readlinkat" => |a| observe(descriptor(a[0]), path(a[1]), Access::Entry);
        pathconf = "pathconf" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Entry);
        // Contents.
        fopen = "fopen" => |a| observe_fopen(path(a[0]), path(a[1]));
        freopen = "freopen" => |a| observe_fopen(path(a[0]), path(a[1]));
        // Directory listings.
        opendir = "opendir" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        fdopendir = "fdopendir" => |a| observe_listing_descriptor(descriptor(a[0]));
        scandir = "scandir" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Listing);
        // Canonical resolution.
        realpath = "realpath" => |a| observe(libc::AT_FDCWD, path(a[0]), Access::Resolution);
        // Environment.
        getenv = "getenv" => |a| observe_environment(a[0]);
        setenv = "setenv" => |_a| record_unobservable(Unobservable::EnvironmentWrite);
        unsetenv = "unsetenv" => |_a| record_unobservable(Unobservable::EnvironmentWrite);
        putenv = "putenv" => |_a| record_unobservable(Unobservable::EnvironmentWrite);
        // Working directory and process control.
        chdir = "chdir" => |_a| record_unobservable(Unobservable::WorkingDirectory);
        fchdir = "fchdir" => |_a| record_unobservable(Unobservable::WorkingDirectory);
        chroot = "chroot" => |_a| record_unobservable(Unobservable::ProcessControl);
        fork = "fork" => |_a| record_unobservable(Unobservable::ProcessControl);
        vfork = "vfork" => |_a| record_unobservable(Unobservable::ProcessControl);
        execve = "execve" => |_a| record_unobservable(Unobservable::ProcessControl);
        execv = "execv" => |_a| record_unobservable(Unobservable::ProcessControl);
        execvp = "execvp" => |_a| record_unobservable(Unobservable::ProcessControl);
        fexecve = "fexecve" => |_a| record_unobservable(Unobservable::ProcessControl);
        system = "system" => |_a| record_unobservable(Unobservable::ProcessControl);
        popen = "popen" => |_a| record_unobservable(Unobservable::ProcessControl);
        posix_spawn_file_actions_addchdir_np = "posix_spawn_file_actions_addchdir_np"
            => |_a| record_unobservable(Unobservable::ProcessControl);
        posix_spawn_file_actions_addchdir = "posix_spawn_file_actions_addchdir"
            => |_a| record_unobservable(Unobservable::ProcessControl);
        posix_spawn_file_actions_addfchdir_np = "posix_spawn_file_actions_addfchdir_np"
            => |_a| record_unobservable(Unobservable::ProcessControl);
        posix_spawn_file_actions_addopen = "posix_spawn_file_actions_addopen"
            => |_a| record_unobservable(Unobservable::ProcessControl);
        // Network.
        socket = "socket" => |_a| record_unobservable(Unobservable::Network);
        socketpair = "socketpair" => |_a| record_unobservable(Unobservable::Network);
        connect = "connect" => |_a| record_unobservable(Unobservable::Network);
        getaddrinfo = "getaddrinfo" => |_a| record_unobservable(Unobservable::Network);
        getnameinfo = "getnameinfo" => |_a| record_unobservable(Unobservable::Network);
        gethostbyname = "gethostbyname" => |_a| record_unobservable(Unobservable::Network);
        gethostbyname2 = "gethostbyname2" => |_a| record_unobservable(Unobservable::Network);
        gethostbyaddr = "gethostbyaddr" => |_a| record_unobservable(Unobservable::Network);
        // Loading code.
        dlopen = "dlopen" => |_a| record_unobservable(Unobservable::DynamicLoad);
        mmap = "mmap" => |a| observe_executable_memory(a[2]);
        mprotect = "mprotect" => |a| observe_executable_memory(a[2]);
        // Writes.
        unlink = "unlink" => |_a| record_unobservable(Unobservable::FileWrite);
        unlinkat = "unlinkat" => |_a| record_unobservable(Unobservable::FileWrite);
        rename = "rename" => |_a| record_unobservable(Unobservable::FileWrite);
        renameat = "renameat" => |_a| record_unobservable(Unobservable::FileWrite);
        mkdir = "mkdir" => |_a| record_unobservable(Unobservable::FileWrite);
        mkdirat = "mkdirat" => |_a| record_unobservable(Unobservable::FileWrite);
        rmdir = "rmdir" => |_a| record_unobservable(Unobservable::FileWrite);
        link = "link" => |_a| record_unobservable(Unobservable::FileWrite);
        linkat = "linkat" => |_a| record_unobservable(Unobservable::FileWrite);
        symlink = "symlink" => |_a| record_unobservable(Unobservable::FileWrite);
        symlinkat = "symlinkat" => |_a| record_unobservable(Unobservable::FileWrite);
        chmod = "chmod" => |_a| record_unobservable(Unobservable::FileWrite);
        fchmod = "fchmod" => |_a| record_unobservable(Unobservable::FileWrite);
        fchmodat = "fchmodat" => |_a| record_unobservable(Unobservable::FileWrite);
        chown = "chown" => |_a| record_unobservable(Unobservable::FileWrite);
        fchown = "fchown" => |_a| record_unobservable(Unobservable::FileWrite);
        lchown = "lchown" => |_a| record_unobservable(Unobservable::FileWrite);
        fchownat = "fchownat" => |_a| record_unobservable(Unobservable::FileWrite);
        truncate = "truncate" => |_a| record_unobservable(Unobservable::FileWrite);
        ftruncate = "ftruncate" => |_a| record_unobservable(Unobservable::FileWrite);
        utimes = "utimes" => |_a| record_unobservable(Unobservable::FileWrite);
        futimes = "futimes" => |_a| record_unobservable(Unobservable::FileWrite);
        utimensat = "utimensat" => |_a| record_unobservable(Unobservable::FileWrite);
        futimens = "futimens" => |_a| record_unobservable(Unobservable::FileWrite);
        mkstemp = "mkstemp" => |_a| record_unobservable(Unobservable::FileWrite);
        mkostemp = "mkostemp" => |_a| record_unobservable(Unobservable::FileWrite);
        mkstemps = "mkstemps" => |_a| record_unobservable(Unobservable::FileWrite);
        mkdtemp = "mkdtemp" => |_a| record_unobservable(Unobservable::FileWrite);
        tmpfile = "tmpfile" => |_a| record_unobservable(Unobservable::FileWrite);
        mkfifo = "mkfifo" => |_a| record_unobservable(Unobservable::FileWrite);
        mknod = "mknod" => |_a| record_unobservable(Unobservable::FileWrite);
        // Host identity.
        uname = "uname" => |_a| record_unobservable(Unobservable::HostState);
        gethostname = "gethostname" => |_a| record_unobservable(Unobservable::HostState);
        getpwuid = "getpwuid" => |_a| record_unobservable(Unobservable::HostState);
        getpwuid_r = "getpwuid_r" => |_a| record_unobservable(Unobservable::HostState);
        getpwnam = "getpwnam" => |_a| record_unobservable(Unobservable::HostState);
        getpwnam_r = "getpwnam_r" => |_a| record_unobservable(Unobservable::HostState);
        getgrgid = "getgrgid" => |_a| record_unobservable(Unobservable::HostState);
        getgrgid_r = "getgrgid_r" => |_a| record_unobservable(Unobservable::HostState);
        getgrnam = "getgrnam" => |_a| record_unobservable(Unobservable::HostState);
        getgrnam_r = "getgrnam_r" => |_a| record_unobservable(Unobservable::HostState);
        getlogin = "getlogin" => |_a| record_unobservable(Unobservable::HostState);
        getlogin_r = "getlogin_r" => |_a| record_unobservable(Unobservable::HostState);
        getloadavg = "getloadavg" => |_a| record_unobservable(Unobservable::HostState);
        getifaddrs = "getifaddrs" => |_a| record_unobservable(Unobservable::HostState);
        statvfs = "statvfs" => |_a| record_unobservable(Unobservable::HostState);
        fstatvfs = "fstatvfs" => |_a| record_unobservable(Unobservable::HostState);
        statfs = "statfs" => |_a| record_unobservable(Unobservable::HostState);
        fstatfs = "fstatfs" => |_a| record_unobservable(Unobservable::HostState);
    }
    explicit {
        open = "open";
        openat = "openat";
        posix_spawn = "posix_spawn";
        posix_spawnp = "posix_spawnp";
        dlsym = "dlsym";
    }
}

// Platform-specific symbols reuse the same observation through thin wrappers in the platform module.
pub(super) use platform::PLATFORM_PURE;

mod explicit {
    use super::*;

    /// # Safety
    ///
    /// `original` must hold the address of the C library's `open` (no directory) or `openat`, and the arguments must be valid for it.
    unsafe fn forward_open(
        original: &AtomicUsize,
        directory: Option<c_int>,
        path: *const c_char,
        flags: c_int,
        mode: c_int,
    ) -> c_int {
        // SAFETY: rebind installed this hook only in a slot that held the original.
        unsafe {
            match directory {
                None => {
                    let function: unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int =
                        std::mem::transmute(super::original(original));
                    function(path, flags, mode)
                }
                Some(directory) => {
                    let function: unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int =
                        std::mem::transmute(super::original(original));
                    function(directory, path, flags, mode)
                }
            }
        }
    }

    /// Whether `open` reads a mode argument for these flags.
    pub(in super::super) fn takes_mode(flags: c_int) -> bool {
        #[cfg(target_os = "linux")]
        return flags & libc::O_CREAT != 0 || flags & libc::O_TMPFILE == libc::O_TMPFILE;
        #[cfg(not(target_os = "linux"))]
        return flags & libc::O_CREAT != 0;
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn open(path: *const c_char, flags: c_int, mut arguments: ...) -> c_int {
        // SAFETY: the C library reads the mode argument exactly when the flags ask for it.
        let mode = if takes_mode(flags) {
            unsafe { arguments.next_arg::<c_int>() }
        } else {
            0
        };
        if observed_process() {
            observe_open(libc::AT_FDCWD, path, flags);
        }
        unsafe { forward_open(&originals::open, None, path, flags, mode) }
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn openat(
        directory: c_int,
        path: *const c_char,
        flags: c_int,
        mut arguments: ...
    ) -> c_int {
        // SAFETY: as for `open`.
        let mode = if takes_mode(flags) {
            unsafe { arguments.next_arg::<c_int>() }
        } else {
            0
        };
        if observed_process() {
            observe_open(directory, path, flags);
        }
        unsafe { forward_open(&originals::openat, Some(directory), path, flags, mode) }
    }

    type Spawn = unsafe extern "C" fn(
        *mut libc::pid_t,
        *const c_char,
        *const libc::posix_spawn_file_actions_t,
        *const libc::posix_spawnattr_t,
        *const *mut c_char,
        *const *mut c_char,
    ) -> c_int;

    /// Record a spawn that inherits the environment and substitute the real environment array.
    ///
    /// # Safety
    ///
    /// `original` must hold the address of the C library's `posix_spawn` or `posix_spawnp`, and the arguments must be valid for it.
    unsafe fn spawn(
        original: &AtomicUsize,
        process: *mut libc::pid_t,
        program: *const c_char,
        actions: *const libc::posix_spawn_file_actions_t,
        attributes: *const libc::posix_spawnattr_t,
        arguments: *const *mut c_char,
        environment: *const *mut c_char,
    ) -> c_int {
        let guarded = ENVIRONMENT_GUARD.array.load(Ordering::Acquire);
        let inherited = !guarded.is_null() && environment == guarded.cast_const();
        if observed_process() {
            match (inherited, c_string(program), argument_vector(arguments)) {
                (true, Some(program), Some(arguments)) => record_spawn(program, arguments),
                (true, _, _) => record_unobservable(Unobservable::PathUnavailable),
                (false, _, _) => record_unobservable(Unobservable::ProcessControl),
            }
        }
        let environment = if inherited {
            super::super::real_environment()
        } else {
            environment
        };
        // SAFETY: rebind installed this hook only in a slot that held the original.
        unsafe {
            let function: Spawn = std::mem::transmute(super::original(original));
            function(process, program, actions, attributes, arguments, environment)
        }
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn posix_spawn(
        process: *mut libc::pid_t,
        program: *const c_char,
        actions: *const libc::posix_spawn_file_actions_t,
        attributes: *const libc::posix_spawnattr_t,
        arguments: *const *mut c_char,
        environment: *const *mut c_char,
    ) -> c_int {
        unsafe {
            spawn(
                &originals::posix_spawn,
                process,
                program,
                actions,
                attributes,
                arguments,
                environment,
            )
        }
    }

    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn posix_spawnp(
        process: *mut libc::pid_t,
        program: *const c_char,
        actions: *const libc::posix_spawn_file_actions_t,
        attributes: *const libc::posix_spawnattr_t,
        arguments: *const *mut c_char,
        environment: *const *mut c_char,
    ) -> c_int {
        unsafe {
            spawn(
                &originals::posix_spawnp,
                process,
                program,
                actions,
                attributes,
                arguments,
                environment,
            )
        }
    }

    /// Resolve a symbol the way the macro's import slots are resolved: through the hook for a hooked name.
    ///
    /// # Safety
    ///
    /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
    pub(super) unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
        // SAFETY: rebind installed this hook only in a slot that held the original.
        let resolved = unsafe {
            let function: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
                std::mem::transmute(super::original(&originals::dlsym));
            function(handle, name)
        };
        if resolved.is_null() || !observed_process() {
            return resolved;
        }
        let Some(symbol) = c_string(name) else {
            record_unobservable(Unobservable::ImportUnclassified);
            return resolved;
        };
        match classify(&symbol) {
            Class::Pure => resolved,
            Class::Hooked(hook) if original(hook.original) == resolved as usize => hook.replacement() as *mut c_void,
            Class::Hooked(_) | Class::Unclassified => {
                record_unobservable_import(format!("{symbol} (dlsym)"));
                resolved
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[path = "hooks_linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "hooks_macos.rs"]
mod platform;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    pub(in super::super) const PLATFORM_PURE: &[&str] = &[];

    pub(in super::super) fn platform_hooks() -> &'static [super::Hook] {
        &[]
    }
}

/// Functions with no effect a result could depend on, shared by both platforms.
///
/// Memory, strings, threads, locks, signals, time, randomness, unwinding, and operations on descriptors the
/// macro already opened through an observed path belong here. Keep the list sorted.
const PURE: &[&str] = &[
    "_Unwind_Backtrace",
    "_Unwind_DeleteException",
    "_Unwind_FindEnclosingFunction",
    "_Unwind_GetCFA",
    "_Unwind_GetDataRelBase",
    "_Unwind_GetGR",
    "_Unwind_GetIP",
    "_Unwind_GetIPInfo",
    "_Unwind_GetLanguageSpecificData",
    "_Unwind_GetRegionStart",
    "_Unwind_GetTextRelBase",
    "_Unwind_RaiseException",
    "_Unwind_Resume",
    "_Unwind_SetGR",
    "_Unwind_SetIP",
    "_exit",
    "abort",
    "acos",
    "acosf",
    "asin",
    "asinf",
    "atan",
    "atan2",
    "atan2f",
    "atanf",
    "bcmp",
    "bzero",
    "calloc",
    "cbrt",
    "cbrtf",
    "ceil",
    "ceilf",
    "clock_getres",
    "clock_gettime",
    "clock_nanosleep",
    "close",
    "closedir",
    "cos",
    "cosf",
    "cosh",
    "coshf",
    "dirfd",
    "dladdr",
    "dlclose",
    "dlerror",
    "dup",
    "dup2",
    "erf",
    "erfc",
    "erfcf",
    "erff",
    "exit",
    "exp",
    "exp2",
    "exp2f",
    "expf",
    "expm1",
    "expm1f",
    "fabs",
    "fabsf",
    "fclose",
    "fcntl",
    "fdim",
    "fdimf",
    "feof",
    "ferror",
    "fflush",
    "fgets",
    "fileno",
    "floor",
    "floorf",
    "fma",
    "fmaf",
    "fmax",
    "fmaxf",
    "fmin",
    "fminf",
    "fmod",
    "fmodf",
    "fprintf",
    "fputc",
    "fputs",
    "fread",
    "free",
    "freeaddrinfo",
    "frexp",
    "frexpf",
    "fseek",
    "fseeko",
    "fstat",
    "fsync",
    "ftell",
    "ftello",
    "fwrite",
    "gai_strerror",
    "getc",
    "getcwd",
    "getegid",
    "getentropy",
    "geteuid",
    "getgid",
    "getpagesize",
    "getpeername",
    "getpid",
    "getppid",
    "getrlimit",
    "getrusage",
    "getsockname",
    "getsockopt",
    "gettimeofday",
    "getuid",
    "hypot",
    "hypotf",
    "ioctl",
    "isatty",
    "kill",
    "ldexp",
    "ldexpf",
    "lgamma",
    "lgammaf",
    "localtime_r",
    "log",
    "log10",
    "log10f",
    "log1p",
    "log1pf",
    "log2",
    "log2f",
    "logf",
    "lseek",
    "madvise",
    "malloc",
    "memchr",
    "memcmp",
    "memcpy",
    "memmove",
    "memrchr",
    "memset",
    "modf",
    "modff",
    "munmap",
    "nanosleep",
    "nextafter",
    "nextafterf",
    "pipe",
    "poll",
    "posix_memalign",
    "posix_spawn_file_actions_addclose",
    "posix_spawn_file_actions_adddup2",
    "posix_spawn_file_actions_destroy",
    "posix_spawn_file_actions_init",
    "posix_spawnattr_destroy",
    "posix_spawnattr_init",
    "posix_spawnattr_setflags",
    "posix_spawnattr_setpgroup",
    "posix_spawnattr_setsigdefault",
    "posix_spawnattr_setsigmask",
    "pow",
    "powf",
    "pread",
    "printf",
    "pthread_atfork",
    "pthread_attr_destroy",
    "pthread_attr_getguardsize",
    "pthread_attr_getstack",
    "pthread_attr_init",
    "pthread_attr_setstacksize",
    "pthread_cond_broadcast",
    "pthread_cond_destroy",
    "pthread_cond_init",
    "pthread_cond_signal",
    "pthread_cond_timedwait",
    "pthread_cond_wait",
    "pthread_condattr_destroy",
    "pthread_condattr_init",
    "pthread_condattr_setclock",
    "pthread_create",
    "pthread_detach",
    "pthread_equal",
    "pthread_getspecific",
    "pthread_join",
    "pthread_key_create",
    "pthread_key_delete",
    "pthread_kill",
    "pthread_mutex_destroy",
    "pthread_mutex_init",
    "pthread_mutex_lock",
    "pthread_mutex_trylock",
    "pthread_mutex_unlock",
    "pthread_mutexattr_destroy",
    "pthread_mutexattr_init",
    "pthread_mutexattr_settype",
    "pthread_once",
    "pthread_rwlock_destroy",
    "pthread_rwlock_rdlock",
    "pthread_rwlock_unlock",
    "pthread_rwlock_wrlock",
    "pthread_self",
    "pthread_setname_np",
    "pthread_setspecific",
    "pthread_sigmask",
    "putc",
    "puts",
    "raise",
    "read",
    "readdir",
    "readdir_r",
    "readv",
    "realloc",
    "recv",
    "recvfrom",
    "recvmsg",
    "remainder",
    "remainderf",
    "rewinddir",
    "round",
    "roundf",
    "sched_yield",
    "send",
    "sendmsg",
    "sendto",
    "setgid",
    "setgroups",
    "setpgid",
    "setsid",
    "setsockopt",
    "setuid",
    "shutdown",
    "sigaction",
    "sigaddset",
    "sigaltstack",
    "sigemptyset",
    "signal",
    "sigprocmask",
    "sin",
    "sincos",
    "sinf",
    "sinh",
    "sinhf",
    "snprintf",
    "sprintf",
    "sqrt",
    "sqrtf",
    "strchr",
    "strcmp",
    "strerror",
    "strerror_r",
    "strlen",
    "strncmp",
    "strnlen",
    "strrchr",
    "sysconf",
    "tan",
    "tanf",
    "tanh",
    "tanhf",
    "tgamma",
    "tgammaf",
    "time",
    "trunc",
    "truncf",
    "usleep",
    "vfprintf",
    "vsnprintf",
    "waitid",
    "waitpid",
    "write",
    "writev",
];

/// The address the macro would load from its own `environ` slot or from `_NSGetEnviron`.
pub(super) fn guarded_environment_variable() -> usize {
    std::ptr::addr_of!(ENVIRONMENT_GUARD.variable) as usize
}
