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

// `forwarding` hooks observe their arguments. `effects` hooks record one effect that no key binds, whatever
// the arguments; the system-call table names the same effect for the kernel operation.
macro_rules! hooks {
    (
        forwarding { $($forward:ident = $forward_symbol:literal => |$arguments:ident| $observe:expr;)* }
        effects { $($effect:ident = $effect_symbol:literal => $reason:ident;)* }
        explicit { $($explicit:ident = $explicit_symbol:literal;)* }
    ) => {
        mod originals {
            use std::sync::atomic::AtomicUsize;
            $(#[allow(non_upper_case_globals)] pub(super) static $forward: AtomicUsize = AtomicUsize::new(0);)*
            $(#[allow(non_upper_case_globals)] pub(super) static $effect: AtomicUsize = AtomicUsize::new(0);)*
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
            $(
                #[allow(non_snake_case)]
                /// # Safety
                ///
                /// Call only through an import slot that `rebind` pointed at this hook, with arguments that are valid for the original C function.
                pub(super) unsafe extern "C" fn $effect(
                    a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize,
                ) -> usize {
                    if observed_process() {
                        record_unobservable(Unobservable::$reason);
                    }
                    // SAFETY: resolve_originals stored this symbol's address, and rebind only installs the
                    // hook in a slot that held it.
                    let function: Forwarder = unsafe { std::mem::transmute(original(&originals::$effect)) };
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
                    symbol: $effect_symbol,
                    function: forwarding::$effect as *const () as usize,
                    original: &originals::$effect,
                },)*
                $(Hook {
                    symbol: $explicit_symbol,
                    function: explicit::$explicit as *const () as usize,
                    original: &originals::$explicit,
                },)*
            ])
        }

        /// The effect that each `effects` hook records.
        #[cfg(test)]
        pub(super) fn effect_hooks() -> &'static [(&'static str, Unobservable)] {
            &[$(($effect_symbol, Unobservable::$reason),)*]
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
        // Loading code.
        mmap = "mmap" => |a| observe_executable_memory(a[2]);
        mprotect = "mprotect" => |a| observe_executable_memory(a[2]);
    }
    effects {
        setenv = "setenv" => EnvironmentWrite;
        unsetenv = "unsetenv" => EnvironmentWrite;
        putenv = "putenv" => EnvironmentWrite;
        // Working directory and process control.
        chdir = "chdir" => WorkingDirectory;
        fchdir = "fchdir" => WorkingDirectory;
        chroot = "chroot" => ProcessControl;
        fork = "fork" => ProcessControl;
        vfork = "vfork" => ProcessControl;
        execve = "execve" => ProcessControl;
        execv = "execv" => ProcessControl;
        execvp = "execvp" => ProcessControl;
        fexecve = "fexecve" => ProcessControl;
        system = "system" => ProcessControl;
        popen = "popen" => ProcessControl;
        posix_spawn_file_actions_addchdir_np = "posix_spawn_file_actions_addchdir_np" => ProcessControl;
        posix_spawn_file_actions_addchdir = "posix_spawn_file_actions_addchdir" => ProcessControl;
        posix_spawn_file_actions_addfchdir_np = "posix_spawn_file_actions_addfchdir_np" => ProcessControl;
        posix_spawn_file_actions_addopen = "posix_spawn_file_actions_addopen" => ProcessControl;
        // A forked child sets its identity before it executes another program; hooks record nothing there.
        setuid = "setuid" => ProcessControl;
        setgid = "setgid" => ProcessControl;
        setgroups = "setgroups" => ProcessControl;
        setpgid = "setpgid" => ProcessControl;
        setsid = "setsid" => ProcessControl;
        // Network. A socket can carry data from any peer, and `recvmsg` can receive another process's
        // descriptors, so every socket operation is a network effect, not only opening one.
        socket = "socket" => Network;
        socketpair = "socketpair" => Network;
        connect = "connect" => Network;
        bind = "bind" => Network;
        listen = "listen" => Network;
        accept = "accept" => Network;
        send = "send" => Network;
        sendto = "sendto" => Network;
        sendmsg = "sendmsg" => Network;
        recv = "recv" => Network;
        recvfrom = "recvfrom" => Network;
        recvmsg = "recvmsg" => Network;
        getsockopt = "getsockopt" => Network;
        setsockopt = "setsockopt" => Network;
        getsockname = "getsockname" => Network;
        getpeername = "getpeername" => Network;
        shutdown = "shutdown" => Network;
        getaddrinfo = "getaddrinfo" => Network;
        getnameinfo = "getnameinfo" => Network;
        gethostbyname = "gethostbyname" => Network;
        gethostbyname2 = "gethostbyname2" => Network;
        gethostbyaddr = "gethostbyaddr" => Network;
        // Loading code.
        dlopen = "dlopen" => DynamicLoad;
        // Writes.
        unlink = "unlink" => FileWrite;
        unlinkat = "unlinkat" => FileWrite;
        rename = "rename" => FileWrite;
        renameat = "renameat" => FileWrite;
        mkdir = "mkdir" => FileWrite;
        mkdirat = "mkdirat" => FileWrite;
        rmdir = "rmdir" => FileWrite;
        link = "link" => FileWrite;
        linkat = "linkat" => FileWrite;
        symlink = "symlink" => FileWrite;
        symlinkat = "symlinkat" => FileWrite;
        chmod = "chmod" => FileWrite;
        fchmod = "fchmod" => FileWrite;
        fchmodat = "fchmodat" => FileWrite;
        chown = "chown" => FileWrite;
        fchown = "fchown" => FileWrite;
        lchown = "lchown" => FileWrite;
        fchownat = "fchownat" => FileWrite;
        truncate = "truncate" => FileWrite;
        ftruncate = "ftruncate" => FileWrite;
        utimes = "utimes" => FileWrite;
        futimes = "futimes" => FileWrite;
        utimensat = "utimensat" => FileWrite;
        futimens = "futimens" => FileWrite;
        mkstemp = "mkstemp" => FileWrite;
        mkostemp = "mkostemp" => FileWrite;
        mkstemps = "mkstemps" => FileWrite;
        mkdtemp = "mkdtemp" => FileWrite;
        tmpfile = "tmpfile" => FileWrite;
        mkfifo = "mkfifo" => FileWrite;
        mknod = "mknod" => FileWrite;
        // Host identity.
        uname = "uname" => HostState;
        gethostname = "gethostname" => HostState;
        getpwuid = "getpwuid" => HostState;
        getpwuid_r = "getpwuid_r" => HostState;
        getpwnam = "getpwnam" => HostState;
        getpwnam_r = "getpwnam_r" => HostState;
        getgrgid = "getgrgid" => HostState;
        getgrgid_r = "getgrgid_r" => HostState;
        getgrnam = "getgrnam" => HostState;
        getgrnam_r = "getgrnam_r" => HostState;
        getlogin = "getlogin" => HostState;
        getlogin_r = "getlogin_r" => HostState;
        getloadavg = "getloadavg" => HostState;
        getifaddrs = "getifaddrs" => HostState;
        // System configuration strings, such as the per-user temporary directory.
        confstr = "confstr" => HostState;
        statvfs = "statvfs" => HostState;
        fstatvfs = "fstatvfs" => HostState;
        statfs = "statfs" => HostState;
        fstatfs = "fstatfs" => HostState;
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
/// macro already opened through an observed path belong here. Event queues, such as epoll, eventfd, and
/// kqueue, also belong here: they create descriptors without a path and report only on descriptors the macro
/// already holds. Socket operations do not; they are network effects. A runtime that a macro links but never
/// starts therefore imports only pure or hooked functions.
///
/// The system-call table in `seccomp.rs` classifies each kernel operation as these tables classify its C
/// library functions; a test enforces it. Keep the list sorted.
const PURE: &[&str] = &[
    "_Exit",
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
    "fdatasync",
    "fdim",
    "fdimf",
    "feof",
    "ferror",
    "fflush",
    "fgets",
    "fileno",
    "flock",
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
    "getpid",
    "getppid",
    "getrlimit",
    "getrusage",
    "gettimeofday",
    "getuid",
    "hypot",
    "hypotf",
    "ioctl",
    "isatty",
    "kill",
    "killpg",
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
    "pause",
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
    "pselect",
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
    "pwrite",
    "raise",
    "read",
    "readdir",
    "readdir_r",
    "readv",
    "realloc",
    "remainder",
    "remainderf",
    "rewinddir",
    "round",
    "roundf",
    "sched_yield",
    "select",
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
    "sleep",
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
    "wait",
    "wait4",
    "waitid",
    "waitpid",
    "write",
    "writev",
];

/// The address the macro would load from its own `environ` slot or from `_NSGetEnviron`.
pub(super) fn guarded_environment_variable() -> usize {
    std::ptr::addr_of!(ENVIRONMENT_GUARD.variable) as usize
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// How the import tables treat one symbol.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(in super::super) enum ImportClass {
        Pure,
        /// Every call records this effect.
        Effect(Unobservable),
        /// A hook observes each call's arguments.
        Observed,
        Unclassified,
    }

    pub(in super::super) fn import_class(name: &str) -> ImportClass {
        match classify(name) {
            Class::Pure => ImportClass::Pure,
            Class::Unclassified => ImportClass::Unclassified,
            Class::Hooked(_) => effect_hooks()
                .iter()
                .chain(platform::effect_hooks())
                .find(|(symbol, _)| *symbol == name)
                .map_or(ImportClass::Observed, |(_, effect)| ImportClass::Effect(*effect)),
        }
    }

    #[test]
    fn classification_tables_are_sorted_and_name_each_symbol_once() {
        for table in [PURE, PLATFORM_PURE] {
            if let Some(pair) = table.windows(2).find(|pair| pair[0] >= pair[1]) {
                panic!("pure table is not strictly sorted at {pair:?}");
            }
        }
        let mut classified = BTreeSet::new();
        for symbol in PURE
            .iter()
            .chain(PLATFORM_PURE)
            .copied()
            .chain(all_hooks().map(|hook| hook.symbol))
        {
            assert!(classified.insert(symbol), "{symbol} is classified twice");
        }
    }

    /// Imports that the standard library and common asynchronous runtimes (tokio, mio, socket2) put into a
    /// procedural-macro library whether or not the macro calls them. Before each was classified, linking the
    /// code that imports it made every crate that loads the macro bypass reuse.
    #[test]
    fn linked_runtime_imports_are_classified() {
        use ImportClass::{Effect, Pure};
        #[cfg(target_os = "macos")]
        use Unobservable::FileWrite;
        use Unobservable::{HostState, Network};

        let mut expected = vec![
            ("accept", Effect(Network)),
            ("bind", Effect(Network)),
            ("listen", Effect(Network)),
            ("recv", Effect(Network)),
            ("send", Effect(Network)),
            ("shutdown", Effect(Network)),
            ("confstr", Effect(HostState)),
            ("fdatasync", Pure),
            ("flock", Pure),
            ("killpg", Pure),
            ("pwrite", Pure),
        ];
        #[cfg(target_os = "linux")]
        expected.extend([
            ("accept4", Effect(Network)),
            ("__libc_current_sigrtmax", Pure),
            // glibc's `pthread_atfork` is a static stub that imports `__register_atfork`.
            ("__register_atfork", Pure),
            ("epoll_create1", Pure),
            ("epoll_ctl", Pure),
            ("epoll_wait", Pure),
            ("eventfd", Pure),
            ("preadv", Pure),
            ("pwritev", Pure),
            ("sendfile", Pure),
            ("sendfile64", Pure),
            ("splice", Pure),
        ]);
        #[cfg(target_os = "macos")]
        expected.extend([
            ("getpeereid", Effect(Network)),
            ("setattrlist", Effect(FileWrite)),
            ("fsetattrlist", Effect(FileWrite)),
            ("copyfile_state_alloc", Pure),
            ("copyfile_state_free", Pure),
            ("copyfile_state_get", Pure),
            ("dispatch_time", Pure),
            ("kevent", Pure),
            ("kqueue", Pure),
            ("mach_wait_until", Pure),
            ("pthread_cond_timedwait_relative_np", Pure),
            ("sendfile", Pure),
        ]);
        for (symbol, class) in expected {
            assert_eq!(import_class(symbol), class, "{symbol}");
        }
    }
}
