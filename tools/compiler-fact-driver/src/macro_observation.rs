//! What loaded procedural macros read while rustc runs them in this process.
//!
//! A procedural macro is a shared library that rustc loads and calls during expansion. The macro links its
//! own copy of the Rust standard library, so it reaches the operating system only through its own imports
//! of the C library, through system calls it issues itself, or through code it loads or generates.
//!
//! When the platform loader adds a macro image, and before the image's initializers run where the platform
//! allows it, the driver instruments that image only:
//!
//! 1. Every imported symbol is classified. An unclassified import, or a shared-library dependency other than
//!    the system C library, makes the observation incomplete.
//! 2. Imports that read the filesystem or the environment are rebound in that image's own import slots to
//!    hooks that record the access and then call the original function. Rustc's own calls are never
//!    rebound, so they are neither recorded nor slowed.
//! 3. Imports whose effect no key can bind, such as starting a process with a changed environment,
//!    networking, or writing a file, are rebound to hooks that record the named effect and then call the
//!    original function. Compilation behavior never changes; only reuse eligibility does.
//! 4. The macro sees a guarded copy of the environment array. Starting a process passes the real array on;
//!    walking the copy records that the whole environment was read.
//! 5. System calls issued without the C library are observed by the kernel on Linux (`seccomp`); macOS has
//!    no unprivileged equivalent, so a macro image that contains a system-call instruction is reported.
//!
//! Cargo-Rail decides which recorded paths and variables it can bind and which effects bypass reuse.

#![allow(
    unsafe_code,
    reason = "observing a loaded library requires rebinding its import slots and calling the C library"
)]

use std::collections::BTreeSet;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use crate::native_input_protocol::{
    MAX_MACRO_ENVIRONMENT_NAME_BYTES, MAX_MACRO_ENVIRONMENT_READS, MAX_MACRO_IMPORT_NAME_BYTES, MAX_MACRO_PATH_BYTES,
    MAX_MACRO_PATH_READS, MAX_MACRO_SPAWN_ARGUMENTS, MAX_MACRO_SPAWNS, MAX_MACRO_UNOBSERVABLE_IMPORTS,
    NativeMacroObservation, NativeMacroPathAccess, NativeMacroPathRead, NativeMacroSpawn, NativeMacroUnobservable,
};

#[cfg(target_os = "linux")]
mod elf;
mod hooks;
#[cfg(target_os = "macos")]
mod macho;
#[cfg(target_os = "linux")]
mod seccomp;

use NativeMacroUnobservable as Unobservable;

/// Exported by every procedural-macro image; rustc looks up the full name with the crate's stable identity.
const PROC_MACRO_DECLS_PREFIX: &[u8] = b"__rustc_proc_macro_decls_";

#[derive(Default)]
struct Recorder {
    paths: BTreeSet<NativeMacroPathRead>,
    environment: BTreeSet<String>,
    spawns: BTreeSet<NativeMacroSpawn>,
    unobservable: BTreeSet<NativeMacroUnobservable>,
    /// Imports that made the observation incomplete.
    imports: BTreeSet<String>,
    /// Canonical paths of instrumented macro images.
    images: BTreeSet<String>,
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);
/// Set once `install` has prepared the platform loader hook.
static INSTALLED: AtomicBool = AtomicBool::new(false);

fn with_recorder(record: impl FnOnce(&mut Recorder)) {
    let mut recorder = RECORDER.lock().unwrap_or_else(PoisonError::into_inner);
    record(recorder.get_or_insert_with(Recorder::default));
}

fn record_unobservable(reason: Unobservable) {
    with_recorder(|recorder| {
        recorder.unobservable.insert(reason);
    });
}

/// Record an import that cannot be classified or rebound, naming it within the protocol's bounds.
fn record_unobservable_import(name: String) {
    with_recorder(|recorder| {
        recorder.unobservable.insert(Unobservable::ImportUnclassified);
        if !name.is_empty()
            && name.len() <= MAX_MACRO_IMPORT_NAME_BYTES
            && !name.contains('\0')
            && recorder.imports.len() < MAX_MACRO_UNOBSERVABLE_IMPORTS
        {
            recorder.imports.insert(name);
        }
    });
}

fn record_path(path: String, access: NativeMacroPathAccess) {
    with_recorder(|recorder| {
        if path.len() > MAX_MACRO_PATH_BYTES
            || recorder.paths.len() >= MAX_MACRO_PATH_READS
                && !recorder.paths.contains(&NativeMacroPathRead {
                    path: path.clone(),
                    access,
                })
        {
            recorder.unobservable.insert(Unobservable::ObservationLimit);
            return;
        }
        recorder.paths.insert(NativeMacroPathRead { path, access });
    });
}

fn record_environment(name: &[u8]) {
    let Ok(name) = std::str::from_utf8(name) else {
        record_unobservable(Unobservable::PathUnavailable);
        return;
    };
    with_recorder(|recorder| {
        if name.is_empty() || name.contains('=') {
            // The C library never finds such a name, so the read cannot depend on the environment.
            return;
        }
        if name.len() > MAX_MACRO_ENVIRONMENT_NAME_BYTES
            || recorder.environment.len() >= MAX_MACRO_ENVIRONMENT_READS && !recorder.environment.contains(name)
        {
            recorder.unobservable.insert(Unobservable::ObservationLimit);
            return;
        }
        recorder.environment.insert(name.to_owned());
    });
}

fn record_spawn(program: String, arguments: Vec<String>) {
    with_recorder(|recorder| {
        if program.len() > MAX_MACRO_PATH_BYTES
            || arguments.len() > MAX_MACRO_SPAWN_ARGUMENTS
            || arguments.iter().any(|argument| argument.len() > MAX_MACRO_PATH_BYTES)
            || recorder.spawns.len() >= MAX_MACRO_SPAWNS
        {
            recorder.unobservable.insert(Unobservable::ObservationLimit);
            return;
        }
        recorder.spawns.insert(NativeMacroSpawn { program, arguments });
    });
}

/// Prepare observation before rustc can load a procedural macro.
///
/// Failure here is reported only if a macro loads later, so a crate without macros is unaffected.
pub(crate) fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    hooks::resolve_originals();
    #[cfg(target_os = "macos")]
    macho::install();
    #[cfg(target_os = "linux")]
    elf::install();
}

/// Complete the observation after expansion, when no macro code runs again before the process exits.
///
/// `dynamic_crate_loaded` reports whether rustc's crate graph holds a dynamic library outside the sysroot.
/// It decides only on platforms without a loader hook, where any such library bypasses reuse.
pub(crate) fn finish(dynamic_crate_loaded: bool) -> Option<NativeMacroObservation> {
    let mut recorder = RECORDER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .unwrap_or_default();
    match loaded_macro_images() {
        Some(loaded) if loaded.is_empty() && recorder.images.is_empty() => return None,
        Some(loaded) => {
            if !INSTALLED.load(Ordering::SeqCst)
                || !loaded.is_subset(&recorder.images)
                || !environment_guard_intact()
                || !kernel_observation_complete()
            {
                recorder.unobservable.insert(Unobservable::ObservationUnavailable);
            }
        }
        None if !dynamic_crate_loaded && recorder.images.is_empty() => return None,
        None => {
            recorder.unobservable.insert(Unobservable::ObservationUnavailable);
        }
    }
    if ENVIRONMENT_GUARD.read.load(Ordering::Acquire) {
        recorder.unobservable.insert(Unobservable::EnvironmentEnumeration);
    }
    if ENVIRONMENT_GUARD.written.load(Ordering::Acquire) {
        recorder.unobservable.insert(Unobservable::EnvironmentWrite);
    }
    Some(NativeMacroObservation {
        paths: recorder.paths.into_iter().collect(),
        environment: recorder.environment.into_iter().collect(),
        spawns: recorder.spawns.into_iter().collect(),
        unobservable: recorder.unobservable.into_iter().collect(),
        unobservable_imports: recorder.imports.into_iter().collect(),
    })
}

/// Canonical paths of every procedural-macro image in the process, or `None` without a loader hook.
fn loaded_macro_images() -> Option<BTreeSet<String>> {
    #[cfg(target_os = "macos")]
    return macho::loaded_macro_images();
    #[cfg(target_os = "linux")]
    return elf::loaded_macro_images();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}

/// Whether the kernel observed every system call that macro code issued without the C library.
fn kernel_observation_complete() -> bool {
    #[cfg(target_os = "linux")]
    return seccomp::complete();
    #[cfg(not(target_os = "linux"))]
    true
}

/// One import of a macro image, with every slot the loader bound to it.
struct Import {
    /// The symbol name without the platform's C prefix.
    name: String,
    slots: Vec<*mut usize>,
}

/// What instrumentation needs from a loaded macro image, independent of its object format.
struct MacroImage {
    path: String,
    imports: Vec<Import>,
    /// Shared libraries the image depends on, as the loader names them.
    dependencies: Vec<String>,
    /// The image runs initializers that the platform ran before instrumentation.
    unobserved_initializers: bool,
    /// The image contains a system-call instruction and the platform cannot observe it.
    unobserved_system_calls: bool,
}

/// Classify and rebind one macro image. Called once per image, before any of its code runs where the
/// platform allows it.
fn instrument(image: MacroImage, allowed_dependency: impl Fn(&str) -> bool) {
    let mut reasons = BTreeSet::new();
    if !prepare_environment_guard() {
        reasons.insert(Unobservable::ObservationUnavailable);
    }
    if image
        .dependencies
        .iter()
        .any(|dependency| !allowed_dependency(dependency))
    {
        reasons.insert(Unobservable::DynamicDependency);
    }
    if image.unobserved_initializers {
        reasons.insert(Unobservable::Initializer);
    }
    if image.unobserved_system_calls {
        reasons.insert(Unobservable::RawSystemCall);
    }
    let mut imports = Vec::new();
    for import in &image.imports {
        match hooks::classify(&import.name) {
            hooks::Class::Pure => {}
            hooks::Class::Unclassified => imports.push(import.name.clone()),
            hooks::Class::Hooked(hook) => {
                if import.slots.iter().any(|&slot| hooks::rebind(slot, hook).is_err()) {
                    imports.push(format!("{} (slot not rebound)", import.name));
                }
            }
        }
    }
    with_recorder(|recorder| {
        recorder.images.insert(image.path);
        recorder.unobservable.extend(reasons);
    });
    for name in imports {
        record_unobservable_import(name);
    }
}

/// Resolve a path argument against the working directory or an open directory descriptor.
///
/// Returns `None` after recording why the path cannot be observed.
///
/// # Safety
///
/// `path` must be null or point to a NUL-terminated string.
unsafe fn absolute_path(directory: c_int, path: *const c_char) -> Option<String> {
    if path.is_null() {
        // The C library rejects a null path without touching the filesystem.
        return None;
    }
    // SAFETY: the macro passes this pointer to the C library as a NUL-terminated path.
    absolute_path_bytes(directory, unsafe { CStr::from_ptr(path) }.to_bytes())
}

/// Resolve path bytes against the working directory or an open directory descriptor.
fn absolute_path_bytes(directory: c_int, bytes: &[u8]) -> Option<String> {
    let Ok(spelling) = std::str::from_utf8(bytes) else {
        record_unobservable(Unobservable::PathUnavailable);
        return None;
    };
    if spelling.starts_with('/') {
        return Some(spelling.to_owned());
    }
    let base = if directory == libc::AT_FDCWD {
        current_directory()
    } else {
        descriptor_path(directory)
    };
    let Some(base) = base else {
        record_unobservable(Unobservable::PathUnavailable);
        return None;
    };
    Some(if spelling.is_empty() {
        base
    } else if base.ends_with('/') {
        format!("{base}{spelling}")
    } else {
        format!("{base}/{spelling}")
    })
}

fn current_directory() -> Option<String> {
    std::env::current_dir()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
}

/// The path of an open descriptor, as the kernel names it now.
fn descriptor_path(descriptor: c_int) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut buffer = [0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes, including the terminator, into the buffer.
        if unsafe { libc::fcntl(descriptor, libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
            return None;
        }
        let length = buffer.iter().position(|byte| *byte == 0)?;
        String::from_utf8(buffer[..length].to_vec()).ok()
    }
    #[cfg(target_os = "linux")]
    {
        let target = std::fs::read_link(format!("/proc/self/fd/{descriptor}")).ok()?;
        let target = target.into_os_string().into_string().ok()?;
        // A deleted or anonymous descriptor has no path that another process could read again.
        (target.starts_with('/') && !target.ends_with(" (deleted)")).then_some(target)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = descriptor;
        None
    }
}

/// A guarded copy of the process environment that each macro image sees in place of the real array.
///
/// The pages stay inaccessible until the first read. A read through the copy means the macro walked the
/// environment itself; starting a process passes the pointer on without reading it, so the spawn hook
/// substitutes the real array.
struct EnvironmentGuard {
    array: AtomicPtr<*mut c_char>,
    length: AtomicUsize,
    /// What the macro image loads through its `environ` slot or from `_NSGetEnviron`.
    variable: AtomicPtr<*mut c_char>,
    read: AtomicBool,
    written: AtomicBool,
    prepared: Mutex<bool>,
}

static ENVIRONMENT_GUARD: EnvironmentGuard = EnvironmentGuard {
    array: AtomicPtr::new(std::ptr::null_mut()),
    length: AtomicUsize::new(0),
    variable: AtomicPtr::new(std::ptr::null_mut()),
    read: AtomicBool::new(false),
    written: AtomicBool::new(false),
    prepared: Mutex::new(false),
};

#[cfg(target_os = "macos")]
const GUARD_SIGNALS: [c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];
#[cfg(not(target_os = "macos"))]
const GUARD_SIGNALS: [c_int; 1] = [libc::SIGSEGV];

/// The handlers in place before the guard, written once before the guard handler is installed.
static mut PREVIOUS_HANDLERS: [std::mem::MaybeUninit<libc::sigaction>; 2] =
    [std::mem::MaybeUninit::uninit(), std::mem::MaybeUninit::uninit()];

/// The process environment array, read through this driver's own unrebound import.
fn real_environment() -> *const *mut c_char {
    #[cfg(target_os = "macos")]
    // SAFETY: _NSGetEnviron returns the address of the process environment variable.
    return unsafe { *libc::_NSGetEnviron() }.cast_const();
    #[cfg(not(target_os = "macos"))]
    {
        unsafe extern "C" {
            static environ: *const *mut c_char;
        }
        // SAFETY: the C library owns `environ`; reading the pointer value is always valid.
        unsafe { environ }
    }
}

/// Build the guarded copy of the environment before the first macro image runs.
fn prepare_environment_guard() -> bool {
    let mut prepared = ENVIRONMENT_GUARD
        .prepared
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if *prepared {
        return !ENVIRONMENT_GUARD.array.load(Ordering::Acquire).is_null();
    }
    *prepared = true;
    let environment = real_environment();
    if environment.is_null() {
        return false;
    }
    let mut count = 0usize;
    // SAFETY: the environment array is NULL-terminated.
    while !unsafe { *environment.add(count) }.is_null() {
        count += 1;
    }
    let page = page_size();
    let Some(length) = count
        .checked_add(1)
        .and_then(|entries| entries.checked_mul(size_of::<usize>()))
        .map(|bytes| bytes.div_ceil(page) * page)
    else {
        return false;
    };
    // SAFETY: a new private anonymous mapping; the copy includes the terminating NULL.
    unsafe {
        let region = libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        if region == libc::MAP_FAILED {
            return false;
        }
        let array = region.cast::<*mut c_char>();
        std::ptr::copy_nonoverlapping(environment, array, count + 1);
        let previous = &raw mut PREVIOUS_HANDLERS;
        for (index, signal) in GUARD_SIGNALS.into_iter().enumerate() {
            if libc::sigaction(signal, std::ptr::null(), (*previous)[index].as_mut_ptr()) != 0 {
                libc::munmap(region, length);
                return false;
            }
        }
        ENVIRONMENT_GUARD.length.store(length, Ordering::Release);
        ENVIRONMENT_GUARD.variable.store(array, Ordering::Release);
        ENVIRONMENT_GUARD.array.store(array, Ordering::Release);
        if libc::mprotect(region, length, libc::PROT_NONE) != 0 {
            return false;
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = guard_fault as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in GUARD_SIGNALS {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return false;
            }
        }
    }
    true
}

/// Whether the guard still owns its fault signals, or no longer needs them because the copy was read.
fn environment_guard_intact() -> bool {
    if ENVIRONMENT_GUARD.array.load(Ordering::Acquire).is_null() || ENVIRONMENT_GUARD.read.load(Ordering::Acquire) {
        return true;
    }
    GUARD_SIGNALS.into_iter().all(|signal| {
        // SAFETY: querying a handler writes only the provided structure.
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        let queried = unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) } == 0;
        queried && current.sa_sigaction == guard_fault as *const () as usize
    })
}

/// Record the first read of the guarded copy and let the access proceed; forward every other fault.
extern "C" fn guard_fault(signal: c_int, information: *mut libc::siginfo_t, context: *mut c_void) {
    let start = ENVIRONMENT_GUARD.array.load(Ordering::Acquire) as usize;
    let length = ENVIRONMENT_GUARD.length.load(Ordering::Acquire);
    // SAFETY: the kernel passes valid signal information to an SA_SIGINFO handler.
    let address = unsafe { (*information).si_addr() } as usize;
    if start != 0 && address >= start && address - start < length {
        let protection = if ENVIRONMENT_GUARD.read.swap(true, Ordering::AcqRel) {
            // The copy is already readable, so this fault is a write through it.
            ENVIRONMENT_GUARD.written.store(true, Ordering::Release);
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: mprotect is async-signal-safe and the range is the guard's own mapping.
        unsafe { libc::mprotect(start as *mut c_void, length, protection) };
        return;
    }
    let Some(index) = GUARD_SIGNALS.into_iter().position(|guarded| guarded == signal) else {
        return;
    };
    // SAFETY: the previous handlers were written before this handler was installed and never change.
    let previous = unsafe { &*(&raw const PREVIOUS_HANDLERS).cast::<libc::sigaction>().add(index) };
    if previous.sa_sigaction == libc::SIG_DFL || previous.sa_sigaction == libc::SIG_IGN {
        // Restore the default action; returning retries the access, which then terminates the process as
        // it would have without the guard.
        // SAFETY: resetting a signal disposition is async-signal-safe.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    } else if previous.sa_flags & libc::SA_SIGINFO != 0 {
        // SAFETY: the previous handler was installed with SA_SIGINFO, so it takes three arguments.
        let handler: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) =
            unsafe { std::mem::transmute(previous.sa_sigaction) };
        handler(signal, information, context);
    } else {
        // SAFETY: the previous handler was installed without SA_SIGINFO, so it takes the signal only.
        let handler: extern "C" fn(c_int) = unsafe { std::mem::transmute(previous.sa_sigaction) };
        handler(signal);
    }
}

fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096)
}

/// The bytes of a NUL-terminated C string, without the terminator.
fn c_string_bytes(pointer: *const c_char) -> Option<Vec<u8>> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: callers pass pointers the macro hands to the C library as NUL-terminated strings.
    Some(unsafe { CStr::from_ptr(pointer) }.to_bytes().to_vec())
}

fn c_string(pointer: *const c_char) -> Option<String> {
    c_string_bytes(pointer).and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Read a NULL-terminated argument vector.
fn argument_vector(vector: *const *mut c_char) -> Option<Vec<String>> {
    if vector.is_null() {
        return Some(Vec::new());
    }
    let mut arguments = Vec::new();
    for index in 0..=MAX_MACRO_SPAWN_ARGUMENTS {
        // SAFETY: the vector is NULL-terminated, as posix_spawn requires; the bound stops a runaway read.
        let argument = unsafe { *vector.add(index) };
        if argument.is_null() {
            return Some(arguments);
        }
        arguments.push(c_string(argument)?);
    }
    None
}
