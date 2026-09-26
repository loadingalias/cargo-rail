//! Macro images in Mach-O processes.
//!
//! dyld calls an add-image callback after it binds an image and before it runs the image's initializers,
//! so instrumentation precedes every instruction of the macro. An image's imports live in pointer sections
//! that the indirect symbol table maps to symbol names.

use std::collections::BTreeSet;
use std::ffi::{CStr, c_char, c_int};

use super::{Import, MacroImage, PROC_MACRO_DECLS_PREFIX, instrument};

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_LOAD_DYLIB: u32 = 0xc;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const LC_REEXPORT_DYLIB: u32 = 0x8000_001f;
const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const SECTION_TYPE: u32 = 0xff;
const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;
const S_LAZY_SYMBOL_POINTERS: u32 = 0x7;
const S_ZEROFILL: u32 = 0x1;
const S_GB_ZEROFILL: u32 = 0xc;
const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
const DATA_SEGMENTS: [&[u8]; 5] = [b"__DATA", b"__DATA_CONST", b"__DATA_DIRTY", b"__AUTH", b"__AUTH_CONST"];
const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x0000_0400;
const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_EXT: u8 = 0x01;
const N_SECT: u8 = 0x0e;
const VM_REGION_BASIC_INFO_64: c_int = 9;

#[repr(C)]
struct MachHeader64 {
    magic: u32,
    cputype: i32,
    cpusubtype: i32,
    filetype: u32,
    ncmds: u32,
    sizeofcmds: u32,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct LoadCommand {
    cmd: u32,
    cmdsize: u32,
}

#[repr(C)]
struct SegmentCommand64 {
    cmd: u32,
    cmdsize: u32,
    segname: [u8; 16],
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    maxprot: i32,
    initprot: i32,
    nsects: u32,
    flags: u32,
}

#[repr(C)]
struct Section64 {
    sectname: [u8; 16],
    segname: [u8; 16],
    addr: u64,
    size: u64,
    offset: u32,
    align: u32,
    reloff: u32,
    nreloc: u32,
    flags: u32,
    reserved1: u32,
    reserved2: u32,
    reserved3: u32,
}

#[repr(C)]
struct SymtabCommand {
    cmd: u32,
    cmdsize: u32,
    symoff: u32,
    nsyms: u32,
    stroff: u32,
    strsize: u32,
}

#[repr(C)]
struct DysymtabCommand {
    cmd: u32,
    cmdsize: u32,
    ilocalsym: u32,
    nlocalsym: u32,
    iextdefsym: u32,
    nextdefsym: u32,
    iundefsym: u32,
    nundefsym: u32,
    tocoff: u32,
    ntoc: u32,
    modtaboff: u32,
    nmodtab: u32,
    extrefsymoff: u32,
    nextrefsyms: u32,
    indirectsymoff: u32,
    nindirectsyms: u32,
    extreloff: u32,
    nextrel: u32,
    locreloff: u32,
    nlocrel: u32,
}

#[repr(C)]
struct DylibCommand {
    cmd: u32,
    cmdsize: u32,
    name_offset: u32,
    timestamp: u32,
    current_version: u32,
    compatibility_version: u32,
}

#[repr(C)]
struct Nlist64 {
    n_strx: u32,
    n_type: u8,
    n_sect: u8,
    n_desc: u16,
    n_value: u64,
}

unsafe extern "C" {
    static mach_task_self_: u32;
    fn _dyld_register_func_for_add_image(callback: extern "C" fn(*const MachHeader64, isize));
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_header(index: u32) -> *const MachHeader64;
    fn _dyld_get_image_vmaddr_slide(index: u32) -> isize;
    fn mach_vm_region(
        task: u32,
        address: *mut u64,
        size: *mut u64,
        flavor: c_int,
        information: *mut c_int,
        count: *mut u32,
        object: *mut u32,
    ) -> c_int;
}

pub(super) fn install() {
    // SAFETY: dyld calls the callback for every loaded image now and for every later image.
    unsafe { _dyld_register_func_for_add_image(image_added) };
}

extern "C" fn image_added(header: *const MachHeader64, slide: isize) {
    // SAFETY: dyld passes the header of a mapped and bound image.
    let Some(image) = (unsafe { ParsedImage::parse(header, slide) }) else {
        return;
    };
    if !image.defines_proc_macro {
        return;
    }
    let Some(path) = image_path(header) else {
        super::record_unobservable(super::Unobservable::PathUnavailable);
        return;
    };
    // SAFETY: the parsed image describes the mapped image dyld just added.
    let macro_image = unsafe { image.macro_image(path) };
    instrument(macro_image, |dependency| dependency == "/usr/lib/libSystem.B.dylib");
}

/// The canonical path of the image containing `header`.
fn image_path(header: *const MachHeader64) -> Option<String> {
    let mut information: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: dladdr writes only the provided structure.
    if unsafe { libc::dladdr(header.cast(), &mut information) } == 0 || information.dli_fname.is_null() {
        return None;
    }
    // SAFETY: dladdr returns a NUL-terminated image path owned by dyld.
    let path = unsafe { CStr::from_ptr(information.dli_fname) }.to_str().ok()?;
    std::fs::canonicalize(path).ok()?.into_os_string().into_string().ok()
}

/// Canonical paths of every procedural-macro image loaded in the process.
pub(super) fn loaded_macro_images() -> Option<BTreeSet<String>> {
    let mut images = BTreeSet::new();
    // SAFETY: dyld's image list is safe to walk; an index past the end yields a null header.
    let count = unsafe { _dyld_image_count() };
    for index in 0..count {
        let header = unsafe { _dyld_get_image_header(index) };
        if header.is_null() {
            continue;
        }
        let slide = unsafe { _dyld_get_image_vmaddr_slide(index) };
        // SAFETY: dyld returned the header of a loaded image.
        let parsed = unsafe { ParsedImage::parse(header, slide) }?;
        if parsed.defines_proc_macro {
            images.insert(image_path(header)?);
        }
    }
    Some(images)
}

/// The current protection of the page that holds `address`.
pub(super) fn region_protection(address: usize) -> Option<c_int> {
    let mut region = address as u64;
    let mut size = 0u64;
    let mut information = [0 as c_int; 9];
    let mut count = information.len() as u32;
    let mut object = 0;
    // SAFETY: mach_vm_region writes the basic region information into the provided buffer.
    let status = unsafe {
        mach_vm_region(
            mach_task_self_,
            &mut region,
            &mut size,
            VM_REGION_BASIC_INFO_64,
            information.as_mut_ptr(),
            &mut count,
            &mut object,
        )
    };
    (status == 0 && region <= address as u64 && (address as u64) < region + size).then_some(information[0])
}

struct Segment {
    name: Vec<u8>,
    vmaddr: u64,
    fileoff: u64,
}

struct ParsedImage {
    slide: isize,
    sections: Vec<*const Section64>,
    symbols: *const Nlist64,
    symbol_count: usize,
    strings: *const c_char,
    string_size: usize,
    indirect: *const u32,
    indirect_count: usize,
    dependencies: Vec<String>,
    undefined: std::ops::Range<usize>,
    defines_proc_macro: bool,
}

/// A C symbol without the Mach-O underscore. Linker-provided names such as `dyld_stub_binder` have none.
fn c_symbol(name: &[u8]) -> &[u8] {
    name.strip_prefix(b"_").unwrap_or(name)
}

fn fixed_name(bytes: &[u8; 16]) -> &[u8] {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

impl ParsedImage {
    /// Parse the load commands and link-edit tables of a mapped 64-bit image.
    ///
    /// # Safety
    ///
    /// `header` must point to the header of a mapped and bound image, and `slide` must be that image's slide.
    unsafe fn parse(header: *const MachHeader64, slide: isize) -> Option<Self> {
        // SAFETY: the caller passes the header of a mapped image; every table pointer is derived from its
        // own load commands and stays inside its mapped link-edit segment.
        unsafe {
            if (*header).magic != MH_MAGIC_64 {
                return None;
            }
            let mut command = header.add(1).cast::<LoadCommand>();
            let end = command.cast::<u8>().add((*header).sizeofcmds as usize);
            let mut sections = Vec::new();
            let mut segments = Vec::new();
            let mut symtab = None;
            let mut dysymtab = None;
            let mut dependencies = Vec::new();
            for _ in 0..(*header).ncmds {
                if command.cast::<u8>() >= end || (*command).cmdsize < 8 {
                    return None;
                }
                match (*command).cmd {
                    LC_SEGMENT_64 => {
                        let segment = command.cast::<SegmentCommand64>();
                        segments.push(Segment {
                            name: fixed_name(&(*segment).segname).to_vec(),
                            vmaddr: (*segment).vmaddr,
                            fileoff: (*segment).fileoff,
                        });
                        let first = segment.add(1).cast::<Section64>();
                        for index in 0..(*segment).nsects as usize {
                            sections.push(first.add(index));
                        }
                    }
                    LC_SYMTAB => symtab = Some(command.cast::<SymtabCommand>()),
                    LC_DYSYMTAB => dysymtab = Some(command.cast::<DysymtabCommand>()),
                    LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LAZY_LOAD_DYLIB
                    | LC_LOAD_UPWARD_DYLIB => {
                        let dylib = command.cast::<DylibCommand>();
                        let name = command.cast::<c_char>().add((*dylib).name_offset as usize);
                        dependencies.push(CStr::from_ptr(name).to_string_lossy().into_owned());
                    }
                    _ => {}
                }
                command = command.cast::<u8>().add((*command).cmdsize as usize).cast();
            }
            let (symtab, dysymtab) = (symtab?, dysymtab?);
            let linkedit = segments.iter().find(|segment| segment.name == b"__LINKEDIT")?;
            let base = (linkedit.vmaddr as isize + slide) as usize - linkedit.fileoff as usize;
            let mut image = Self {
                slide,
                sections,
                symbols: (base + (*symtab).symoff as usize) as *const Nlist64,
                symbol_count: (*symtab).nsyms as usize,
                strings: (base + (*symtab).stroff as usize) as *const c_char,
                string_size: (*symtab).strsize as usize,
                indirect: (base + (*dysymtab).indirectsymoff as usize) as *const u32,
                indirect_count: (*dysymtab).nindirectsyms as usize,
                dependencies,
                undefined: (*dysymtab).iundefsym as usize..((*dysymtab).iundefsym + (*dysymtab).nundefsym) as usize,
                defines_proc_macro: false,
            };
            let defined = (*dysymtab).iextdefsym as usize..((*dysymtab).iextdefsym + (*dysymtab).nextdefsym) as usize;
            image.defines_proc_macro = defined.into_iter().any(|index| {
                image.symbol(index).is_some_and(|symbol| {
                    symbol.n_type & N_STAB == 0
                        && symbol.n_type & N_EXT != 0
                        && symbol.n_type & N_TYPE == N_SECT
                        && image
                            .symbol_name(symbol)
                            .and_then(|name| name.strip_prefix(b"_"))
                            .is_some_and(|name| name.starts_with(PROC_MACRO_DECLS_PREFIX))
                })
            });
            Some(image)
        }
    }

    fn symbol(&self, index: usize) -> Option<&Nlist64> {
        // SAFETY: the index is bounded by the symbol table's own count.
        (index < self.symbol_count).then(|| unsafe { &*self.symbols.add(index) })
    }

    fn symbol_name(&self, symbol: &Nlist64) -> Option<&[u8]> {
        let offset = symbol.n_strx as usize;
        if offset >= self.string_size {
            return None;
        }
        // SAFETY: the string table is NUL-terminated within its declared size.
        Some(unsafe { CStr::from_ptr(self.strings.add(offset)) }.to_bytes())
    }

    /// Collect every import and its slots, scan code and data, and describe the image.
    ///
    /// # Safety
    ///
    /// The image that `self` describes must still be mapped.
    unsafe fn macro_image(&self, path: String) -> MacroImage {
        let mut imports = std::collections::BTreeMap::<String, Vec<*mut usize>>::new();
        let mut unreadable = false;
        for index in self.undefined.clone() {
            match self
                .symbol(index)
                .and_then(|symbol| self.symbol_name(symbol))
                .and_then(|name| std::str::from_utf8(c_symbol(name)).ok())
            {
                Some(name) => {
                    imports.entry(name.to_owned()).or_default();
                }
                None => unreadable = true,
            }
        }
        let mut slots = BTreeSet::new();
        let mut system_calls = false;
        let mut data = Vec::new();
        // SAFETY: section addresses and sizes come from the mapped image's own load commands.
        unsafe {
            for &section in &self.sections {
                let kind = (*section).flags & SECTION_TYPE;
                let start = ((*section).addr as isize + self.slide) as usize;
                let size = (*section).size as usize;
                if matches!(kind, S_NON_LAZY_SYMBOL_POINTERS | S_LAZY_SYMBOL_POINTERS) {
                    for slot in 0..size / size_of::<usize>() {
                        let index = (*section).reserved1 as usize + slot;
                        if index >= self.indirect_count {
                            unreadable = true;
                            break;
                        }
                        let symbol = *self.indirect.add(index);
                        if symbol & (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS) != 0 {
                            continue;
                        }
                        let Some(name) = self
                            .symbol(symbol as usize)
                            .and_then(|symbol| self.symbol_name(symbol))
                            .and_then(|name| std::str::from_utf8(c_symbol(name)).ok())
                        else {
                            unreadable = true;
                            continue;
                        };
                        let address = (start as *mut usize).add(slot);
                        slots.insert(address as usize);
                        imports.entry(name.to_owned()).or_default().push(address);
                    }
                    continue;
                }
                let instructions = (*section).flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0;
                if instructions {
                    if contains_system_call(std::slice::from_raw_parts(start as *const u8, size)) {
                        system_calls = true;
                    }
                } else if !matches!(kind, S_ZEROFILL | S_GB_ZEROFILL | S_THREAD_LOCAL_ZEROFILL)
                    && DATA_SEGMENTS.contains(&fixed_name(&(*section).segname))
                {
                    data.push((start, size));
                }
            }
        }
        // A bind outside the pointer sections, such as a function pointer in a constant table, would call a
        // hooked function without its hook. Such a reference makes the import unobservable.
        let hooked = super::hooks::hooked_originals();
        let mut unhooked_reference = false;
        for (start, size) in data {
            let first = start.next_multiple_of(align_of::<usize>());
            let words = (start + size).saturating_sub(first) / size_of::<usize>();
            // SAFETY: the words lie inside a mapped data section of this image.
            let values = unsafe { std::slice::from_raw_parts(first as *const usize, words) };
            if values
                .iter()
                .enumerate()
                .any(|(index, value)| hooked.contains(value) && !slots.contains(&(first + index * size_of::<usize>())))
            {
                unhooked_reference = true;
            }
        }
        let mut imports = imports
            .into_iter()
            .map(|(name, slots)| Import { name, slots })
            .collect::<Vec<_>>();
        if unreadable || unhooked_reference {
            // An import that cannot be named or rebound everywhere cannot be classified.
            imports.push(Import {
                name: String::new(),
                slots: Vec::new(),
            });
        }
        MacroImage {
            path,
            imports,
            dependencies: self.dependencies.clone(),
            // dyld runs initializers after this callback, so they execute instrumented.
            unobserved_initializers: false,
            unobserved_system_calls: system_calls,
        }
    }
}

/// Whether the instruction bytes contain a supervisor call, which reaches the kernel without libSystem.
fn contains_system_call(bytes: &[u8]) -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        // Every A64 instruction is four aligned bytes; `svc #imm16` encodes as 0xd4000001 | imm16 << 5.
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .any(|word| word & 0xffe0_001f == 0xd400_0001)
    }
    #[cfg(target_arch = "x86_64")]
    {
        // x86-64 instructions have no fixed boundary, and a jump can enter the middle of one, so the scan
        // checks every byte offset for `syscall`, `sysenter`, and `int 0x80`-`0x82`. Ordinary code often
        // holds these bytes inside other instructions, so most macro images on this target bypass reuse.
        bytes
            .windows(2)
            .any(|pair| matches!(pair, [0x0f, 0x05 | 0x34] | [0xcd, 0x80..=0x82]))
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        let _ = bytes;
        true
    }
}
