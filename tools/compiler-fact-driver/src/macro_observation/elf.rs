//! Macro images in ELF processes on Linux.
//!
//! The GNU C library has no callback that runs before a new library's initializers. Rustc loads every
//! procedural macro through `dlopen`, so the driver rebinds rustc's own `dlopen` import. Before each load it
//! installs the seccomp filter on the loading thread; after the load it instruments every procedural-macro
//! image the call added. An image whose initializers do more than the C runtime's and the Rust standard
//! library's own setup is reported, because those initializers already ran.

use std::collections::BTreeSet;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::unix::fs::FileExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use super::{Import, MacroImage, PROC_MACRO_DECLS_PREFIX, Unobservable, instrument, record_unobservable};

const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_PLTRELSZ: i64 = 2;
const DT_HASH: i64 = 4;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_STRSZ: i64 = 10;
const DT_INIT: i64 = 12;
const DT_REL: i64 = 17;
const DT_PLTREL: i64 = 20;
const DT_JMPREL: i64 = 23;
const DT_INIT_ARRAY: i64 = 25;
const DT_INIT_ARRAYSZ: i64 = 27;
const DT_PREINIT_ARRAY: i64 = 32;
const DT_GNU_HASH: i64 = 0x6fff_fef5;
const SHN_UNDEF: u16 = 0;
const SHT_SYMTAB: u32 = 2;
const STT_FUNC: u8 = 2;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PF_X: u32 = 1;

/// Initializers of the C runtime and of the compiler's builtins, which only register frame information or
/// read the processor's features from the auxiliary vector. The standard library's own initializer, which
/// records the process arguments, is recognized by its `ARGV_INIT_ARRAY` path.
const STANDARD_INITIALIZERS: [&str; 5] = [
    "__cpu_indicator_init",
    "__init_cpu_features",
    "__init_cpu_features_constructor",
    "frame_dummy",
    "init_have_lse_atomics",
];

/// Libraries a macro image may depend on: the C runtime, whose imports the classification covers.
const SYSTEM_LIBRARIES: [&str; 9] = [
    "libc.so.6",
    "libm.so.6",
    "libdl.so.2",
    "libpthread.so.0",
    "librt.so.1",
    "libutil.so.1",
    "libgcc_s.so.1",
    "ld64.so.1",
    "ld64.so.2",
];

#[repr(C)]
#[derive(Clone, Copy)]
struct Dynamic {
    tag: i64,
    value: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Symbol {
    name: u32,
    info: u8,
    other: u8,
    section: u16,
    value: u64,
    size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Relocation {
    offset: u64,
    info: u64,
    addend: i64,
}

/// One loaded object, as the dynamic loader reports it.
#[derive(Clone)]
struct LoadedImage {
    base: usize,
    name: String,
    executable: Vec<(usize, usize)>,
    dynamic: Option<usize>,
}

/// Executable ranges of instrumented macro images, which the seccomp supervisor attributes calls to.
static MACRO_RANGES: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static RUSTC_DLOPEN: AtomicUsize = AtomicUsize::new(0);

pub(super) fn macro_image_contains(address: usize) -> bool {
    MACRO_RANGES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|&(start, end)| (start..end).contains(&address))
}

/// Rebind rustc's `dlopen` import so every macro load passes through `rustc_dlopen`.
pub(super) fn install() {
    let anchor = rustc_driver::run_compiler as *const () as usize;
    let images = loaded_images();
    let Some(rustc) = images.iter().find(|image| {
        image
            .executable
            .iter()
            .any(|&(start, end)| (start..end).contains(&anchor))
    }) else {
        return;
    };
    // SAFETY: the loader reported this image and its dynamic section.
    let Some(parsed) = (unsafe { ParsedImage::parse(rustc) }) else {
        return;
    };
    let Some(import) = parsed.imports().into_iter().find(|import| import.name == "dlopen") else {
        return;
    };
    let Some(&first) = import.slots.first() else {
        return;
    };
    // SAFETY: the slot belongs to rustc's loaded import table.
    let original = unsafe { first.read_volatile() };
    RUSTC_DLOPEN.store(original, Ordering::Release);
    for slot in import.slots {
        if super::hooks::rebind_slot(slot, original, rustc_dlopen as *const () as usize).is_err() {
            RUSTC_DLOPEN.store(0, Ordering::Release);
            return;
        }
    }
}

/// Rustc's `dlopen`: filter the loading thread, load, then instrument each macro image the load added.
///
/// # Safety
///
/// Call only through rustc's rebound `dlopen` slot, with arguments that are valid for `dlopen`.
unsafe extern "C" fn rustc_dlopen(path: *const c_char, flags: c_int) -> *mut c_void {
    let before = loaded_images();
    super::seccomp::install_on_current_thread(
        &before
            .iter()
            .flat_map(|image| image.executable.iter().copied())
            .collect::<Vec<_>>(),
    );
    // SAFETY: install stored the function the slot held, and rebinding happened only after it succeeded.
    let handle = unsafe {
        let function: unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void =
            std::mem::transmute(RUSTC_DLOPEN.load(Ordering::Acquire));
        function(path, flags)
    };
    let known = before
        .iter()
        .map(|image| (image.base, image.name.clone()))
        .collect::<BTreeSet<_>>();
    for image in loaded_images() {
        if known.contains(&(image.base, image.name.clone())) {
            continue;
        }
        // SAFETY: the loader reported this image after mapping and relocating it.
        let Some(parsed) = (unsafe { ParsedImage::parse(&image) }) else {
            continue;
        };
        if !parsed.defines_proc_macro() {
            continue;
        }
        let Some(path) = std::fs::canonicalize(&image.name)
            .ok()
            .and_then(|path| path.into_os_string().into_string().ok())
        else {
            record_unobservable(Unobservable::PathUnavailable);
            continue;
        };
        MACRO_RANGES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(image.executable.iter().copied());
        let unobserved_initializers = !parsed.initializers_are_standard(&image);
        instrument(
            MacroImage {
                path,
                imports: parsed.imports(),
                dependencies: parsed.needed(),
                unobserved_initializers,
                // The seccomp filter observes system calls from this image.
                unobserved_system_calls: false,
            },
            |dependency| {
                SYSTEM_LIBRARIES.contains(&dependency)
                    || dependency.starts_with("ld-linux") && dependency.contains(".so.")
            },
        );
    }
    handle
}

/// Canonical paths of every procedural-macro image loaded in the process.
pub(super) fn loaded_macro_images() -> Option<BTreeSet<String>> {
    let mut images = BTreeSet::new();
    for image in loaded_images() {
        // SAFETY: the loader reported this image.
        if unsafe { ParsedImage::parse(&image) }.is_some_and(|parsed| parsed.defines_proc_macro()) {
            images.insert(
                std::fs::canonicalize(&image.name)
                    .ok()?
                    .into_os_string()
                    .into_string()
                    .ok()?,
            );
        }
    }
    Some(images)
}

fn loaded_images() -> Vec<LoadedImage> {
    /// # Safety
    ///
    /// Call only from `dl_iterate_phdr`, with `data` pointing to a `Vec<LoadedImage>`.
    unsafe extern "C" fn visit(information: *mut libc::dl_phdr_info, _size: usize, data: *mut c_void) -> c_int {
        // SAFETY: the loader passes a valid description and our vector as data.
        let (information, images) = unsafe { (&*information, &mut *data.cast::<Vec<LoadedImage>>()) };
        let base = information.dlpi_addr as usize;
        let name = if information.dlpi_name.is_null() {
            String::new()
        } else {
            // SAFETY: the loader's image names are NUL-terminated.
            unsafe { CStr::from_ptr(information.dlpi_name) }
                .to_string_lossy()
                .into_owned()
        };
        let mut executable = Vec::new();
        let mut dynamic = None;
        for index in 0..usize::from(information.dlpi_phnum) {
            // SAFETY: the header table has dlpi_phnum entries.
            let header = unsafe { &*information.dlpi_phdr.add(index) };
            let start = base + header.p_vaddr as usize;
            if header.p_type == PT_LOAD && header.p_flags & PF_X != 0 {
                executable.push((start, start + header.p_memsz as usize));
            } else if header.p_type == PT_DYNAMIC {
                dynamic = Some(start);
            }
        }
        images.push(LoadedImage {
            base,
            name,
            executable,
            dynamic,
        });
        0
    }
    let mut images = Vec::new();
    // SAFETY: the callback only reads loader data and appends to the vector it receives.
    unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut images).cast()) };
    images
}

struct ParsedImage {
    base: usize,
    strings: usize,
    string_size: usize,
    symbols: usize,
    symbol_count: usize,
    relocations: Vec<(usize, usize)>,
    needed: Vec<usize>,
    init: Option<usize>,
    init_array: Option<(usize, usize)>,
    unsupported: bool,
}

impl ParsedImage {
    /// Parse the dynamic section of a loaded image in memory.
    ///
    /// # Safety
    ///
    /// `image` must describe an image that the loader has mapped and relocated and still holds.
    unsafe fn parse(image: &LoadedImage) -> Option<Self> {
        let dynamic = image.dynamic?;
        let base = image.base;
        // The loader may leave addresses in the dynamic section unrelocated; an address below the image
        // base is an offset from it.
        let address = |value: u64| {
            let value = value as usize;
            if value < base { base + value } else { value }
        };
        let mut parsed = Self {
            base,
            strings: 0,
            string_size: 0,
            symbols: 0,
            symbol_count: 0,
            relocations: Vec::new(),
            needed: Vec::new(),
            init: None,
            init_array: None,
            unsupported: false,
        };
        let (mut rela, mut rela_size, mut plt, mut plt_size, mut plt_kind) = (0, 0, 0, 0, DT_RELA as u64);
        let (mut hash, mut gnu_hash, mut init_array, mut init_array_size) = (0, 0, 0, 0);
        for index in 0.. {
            // SAFETY: the dynamic section is an array terminated by DT_NULL.
            let entry = unsafe { *(dynamic as *const Dynamic).add(index) };
            match entry.tag {
                DT_NULL => break,
                DT_NEEDED => parsed.needed.push(entry.value as usize),
                DT_STRTAB => parsed.strings = address(entry.value),
                DT_STRSZ => parsed.string_size = entry.value as usize,
                DT_SYMTAB => parsed.symbols = address(entry.value),
                DT_HASH => hash = address(entry.value),
                DT_GNU_HASH => gnu_hash = address(entry.value),
                DT_RELA => rela = address(entry.value),
                DT_RELASZ => rela_size = entry.value as usize,
                DT_JMPREL => plt = address(entry.value),
                DT_PLTRELSZ => plt_size = entry.value as usize,
                DT_PLTREL => plt_kind = entry.value,
                DT_INIT => parsed.init = Some(address(entry.value)),
                DT_INIT_ARRAY => init_array = address(entry.value),
                DT_INIT_ARRAYSZ => init_array_size = entry.value as usize,
                DT_REL | DT_PREINIT_ARRAY => parsed.unsupported = true,
                _ => {}
            }
        }
        if parsed.strings == 0 || parsed.symbols == 0 || plt_kind != DT_RELA as u64 {
            return None;
        }
        // SAFETY: the hash tables and relocation arrays belong to the loaded image.
        parsed.symbol_count = unsafe { symbol_count(hash, gnu_hash)? };
        for (start, size) in [(rela, rela_size), (plt, plt_size)] {
            if start != 0 {
                parsed.relocations.push((start, size / size_of::<Relocation>()));
            }
        }
        if init_array != 0 {
            parsed.init_array = Some((init_array, init_array_size / size_of::<usize>()));
        }
        Some(parsed)
    }

    fn symbol(&self, index: usize) -> Option<Symbol> {
        // SAFETY: the index is bounded by the symbol count derived from the image's hash table.
        (index < self.symbol_count).then(|| unsafe { *(self.symbols as *const Symbol).add(index) })
    }

    fn string(&self, offset: usize) -> Option<&str> {
        if offset >= self.string_size {
            return None;
        }
        // SAFETY: the string table is NUL-terminated within its declared size.
        unsafe { CStr::from_ptr((self.strings + offset) as *const c_char) }
            .to_str()
            .ok()
    }

    fn defines_proc_macro(&self) -> bool {
        (1..self.symbol_count).any(|index| {
            self.symbol(index).is_some_and(|symbol| {
                symbol.section != SHN_UNDEF
                    && self
                        .string(symbol.name as usize)
                        .is_some_and(|name| name.as_bytes().starts_with(PROC_MACRO_DECLS_PREFIX))
            })
        })
    }

    fn needed(&self) -> Vec<String> {
        self.needed
            .iter()
            .map(|offset| self.string(*offset).unwrap_or_default().to_owned())
            .collect()
    }

    /// Every undefined symbol, with each relocation slot that refers to it.
    fn imports(&self) -> Vec<Import> {
        let mut imports = std::collections::BTreeMap::<String, Vec<*mut usize>>::new();
        let mut unreadable = self.unsupported;
        for index in 1..self.symbol_count {
            let Some(symbol) = self.symbol(index) else {
                continue;
            };
            if symbol.section != SHN_UNDEF {
                continue;
            }
            match self.string(symbol.name as usize) {
                Some("") => {}
                Some(name) => {
                    imports.entry(name.to_owned()).or_default();
                }
                None => unreadable = true,
            }
        }
        for &(start, count) in &self.relocations {
            for index in 0..count {
                // SAFETY: the relocation array belongs to the loaded image.
                let relocation = unsafe { *(start as *const Relocation).add(index) };
                let symbol_index = (relocation.info >> 32) as usize;
                if symbol_index == 0 {
                    continue;
                }
                let Some(symbol) = self.symbol(symbol_index) else {
                    unreadable = true;
                    continue;
                };
                if symbol.section != SHN_UNDEF {
                    continue;
                }
                if let Some(slots) = self.string(symbol.name as usize).and_then(|name| imports.get_mut(name)) {
                    // A slot that holds anything but the plain address fails rebinding and is reported.
                    slots.push((self.base + relocation.offset as usize) as *mut usize);
                }
            }
        }
        let mut imports = imports
            .into_iter()
            .map(|(name, slots)| Import { name, slots })
            .collect::<Vec<_>>();
        if unreadable {
            imports.push(Import {
                name: String::new(),
                slots: Vec::new(),
            });
        }
        imports
    }

    /// Whether every initializer is the C runtime's or the Rust standard library's own setup, identified
    /// by the image file's symbol table. They ran before instrumentation, so anything else is unobserved.
    fn initializers_are_standard(&self, image: &LoadedImage) -> bool {
        let Some((start, count)) = self.init_array else {
            return true;
        };
        let functions = (0..count)
            // SAFETY: the initializer array belongs to the loaded image.
            .map(|index| unsafe { *(start as *const usize).add(index) })
            .filter(|address| *address != 0 && *address != usize::MAX)
            .map(|address| address.wrapping_sub(self.base))
            .collect::<Vec<_>>();
        if functions.is_empty() {
            return true;
        }
        let Some(names) = function_names(&image.name, &functions) else {
            return false;
        };
        names
            .iter()
            .all(|name| STANDARD_INITIALIZERS.contains(&name.as_str()) || name.contains("ARGV_INIT_ARRAY"))
    }
}

/// The number of dynamic symbols, from the image's hash table.
///
/// # Safety
///
/// Each nonzero address must point to the loaded image's `DT_HASH` or `DT_GNU_HASH` table.
unsafe fn symbol_count(hash: usize, gnu_hash: usize) -> Option<usize> {
    if hash != 0 {
        // SAFETY: DT_HASH starts with nbucket and nchain; nchain equals the symbol count.
        return Some(unsafe { *(hash as *const u32).add(1) } as usize);
    }
    if gnu_hash == 0 {
        return None;
    }
    // SAFETY: the GNU hash table layout is a header, a bloom filter, buckets, and a chain array.
    unsafe {
        let header = gnu_hash as *const u32;
        let (buckets, offset, bloom) = (*header as usize, *header.add(1) as usize, *header.add(2) as usize);
        let bucket_start = header.add(4).cast::<u8>().add(bloom * size_of::<usize>()).cast::<u32>();
        let last = (0..buckets).map(|index| *bucket_start.add(index) as usize).max()?;
        if last < offset {
            return Some(offset);
        }
        let chain = bucket_start.add(buckets);
        let mut index = last;
        while *chain.add(index - offset) & 1 == 0 {
            index += 1;
        }
        Some(index + 1)
    }
}

/// Names of the functions at these image offsets, from the file's static symbol table.
fn function_names(path: &str, offsets: &[usize]) -> Option<Vec<String>> {
    let file = std::fs::File::open(path).ok()?;
    let read = |offset: u64, length: usize| -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; length];
        file.read_exact_at(&mut bytes, offset).ok()?;
        Some(bytes)
    };
    let header = read(0, 64)?;
    if header.get(..4)? != b"\x7fELF" || header[4] != 2 {
        return None;
    }
    let number = |bytes: &[u8], at: usize, width: usize| -> Option<u64> {
        let slice = bytes.get(at..at + width)?;
        let mut buffer = [0u8; 8];
        if header[5] == 1 {
            buffer[..width].copy_from_slice(slice);
            Some(u64::from_le_bytes(buffer))
        } else {
            buffer[8 - width..].copy_from_slice(slice);
            Some(u64::from_be_bytes(buffer))
        }
    };
    let section_offset = number(&header, 0x28, 8)?;
    let section_size = number(&header, 0x3a, 2)? as usize;
    let section_count = number(&header, 0x3c, 2)? as usize;
    let sections = read(section_offset, section_size.checked_mul(section_count)?)?;
    let section = |index: usize| sections.get(index * section_size..(index + 1) * section_size);
    for index in 0..section_count {
        let entry = section(index)?;
        if number(entry, 4, 4)? as u32 != SHT_SYMTAB {
            continue;
        }
        let strings = section(number(entry, 0x28, 4)? as usize)?;
        let string_table = read(number(strings, 0x18, 8)?, number(strings, 0x20, 8)? as usize)?;
        let symbol_size = number(entry, 0x38, 8)? as usize;
        let symbols = read(number(entry, 0x18, 8)?, number(entry, 0x20, 8)? as usize)?;
        let mut names = Vec::with_capacity(offsets.len());
        for &offset in offsets {
            let name = symbols.chunks_exact(symbol_size).find_map(|symbol| {
                (symbol[4] & 0xf == STT_FUNC && number(symbol, 8, 8)? as usize == offset).then(|| {
                    let start = number(symbol, 0, 4)? as usize;
                    let end = string_table.get(start..)?.iter().position(|byte| *byte == 0)? + start;
                    String::from_utf8(string_table[start..end].to_vec()).ok()
                })?
            })?;
            names.push(name);
        }
        return Some(names);
    }
    None
}
