//! What rustc bundled into an rlib or staticlib, checked against the native-library searches that the
//! observation reports.
//!
//! The observation names each static library that rustc may find itself, and Cargo-Rail binds those
//! searches. Rustc copies every member of a library that it bundles into the archive it writes. A member
//! that this compilation did not generate and that no reported library holds therefore came from a search
//! that nothing binds, so the observation is not published and the wrapper runs without reuse.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{ErrorKind, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use rustc_codegen_ssa::{CompiledModules, CrateInfo};
#[cfg(not(rail_structures_native_lib_kind))]
use rustc_hir::attrs::NativeLibKind;
use rustc_session::Session;
use rustc_session::config::{OutFileName, OutputFilenames};
#[cfg(rail_structures_native_lib_kind)]
use rustc_structures::NativeLibKind;

use crate::CrateType;
use crate::native_input_protocol::NativeLibrarySearch;

/// Rustc's metadata member and the member that lists an rlib's Rust objects.
const GENERATED_MEMBERS: [&[u8]; 2] = [b"lib.rmeta", b"lib.rmeta-link"];
/// Archive writers pad a member to at most this alignment.
const MAX_MEMBER_PADDING: usize = 8;

/// The libraries the observation reported, and whether the archives that rustc wrote hold anything else.
#[derive(Default)]
pub(crate) struct BundleCheck {
    /// The library that each reported search selected.
    pub(crate) reported: Vec<PathBuf>,
    /// Why an archive that this compilation wrote holds a member that the report does not explain.
    pub(crate) unexplained: Option<String>,
}

/// The library that each search selected: its first present candidate.
pub(crate) fn selected_libraries(searches: &[NativeLibrarySearch]) -> Vec<PathBuf> {
    searches
        .iter()
        .filter_map(|search| search.probes.iter().find(|probe| probe.present))
        .map(|probe| Path::new(&probe.directory).join(&probe.file))
        .collect()
}

/// The archives that one link writes and the members that it generates, gathered before rustc links.
pub(crate) struct BundleExpectation {
    /// Each archive, and whether it is a staticlib.
    archives: Vec<(PathBuf, bool)>,
    generated: BTreeSet<Vec<u8>>,
    /// The rlibs whose members a staticlib includes.
    upstream_rlibs: Vec<PathBuf>,
}

impl BundleExpectation {
    /// `None` when the link writes no rlib or staticlib.
    pub(crate) fn new(
        sess: &Session,
        modules: &CompiledModules,
        info: &CrateInfo,
        outputs: &OutputFilenames,
    ) -> Result<Option<Self>, String> {
        if !sess.opts.output_types.should_link() {
            return Ok(None);
        }
        let mut archives = Vec::new();
        for &crate_type in &info.crate_types {
            let staticlib = match crate_type {
                CrateType::Rlib => false,
                CrateType::StaticLib => true,
                _ => continue,
            };
            match rustc_session::output::out_filename(sess, crate_type, outputs, info.local_crate_name) {
                OutFileName::Real(path) => archives.push((path, staticlib)),
                OutFileName::Stdout => return Err("an archive written to standard output cannot be checked".into()),
            }
        }
        if archives.is_empty() {
            return Ok(None);
        }
        let mut generated = GENERATED_MEMBERS
            .map(<[u8]>::to_vec)
            .into_iter()
            .collect::<BTreeSet<_>>();
        for module in modules.modules.iter().chain(&modules.allocator_module) {
            for object in [&module.object, &module.global_asm_object, &module.dwarf_object]
                .into_iter()
                .flatten()
            {
                let name = object.file_name().ok_or("a compiled module has no file name")?;
                generated.insert(name.as_encoded_bytes().to_vec());
            }
        }
        // Rustc generates the import library of a raw-dylib DLL itself; each member names the DLL.
        for library in &info.used_libraries {
            if matches!(library.kind, NativeLibKind::RawDylib { .. }) {
                let name = library.name.as_str();
                let dll = if library.verbatim {
                    name.to_owned()
                } else {
                    format!("{name}.dll")
                };
                generated.insert(dll.into_bytes());
            }
        }
        let upstream_rlibs = info
            .used_crates
            .iter()
            .filter_map(|crate_num| info.used_crate_source.get(crate_num)?.rlib.clone())
            .collect();
        Ok(Some(Self {
            archives,
            generated,
            upstream_rlibs,
        }))
    }

    pub(crate) fn verify(&self, reported: &[PathBuf]) -> Result<(), String> {
        verify_archives(&self.archives, &self.generated, &self.upstream_rlibs, reported)
    }
}

/// Fail when an archive holds a member that the compilation did not generate and that no reported library
/// holds. A staticlib also includes the members of its upstream rlibs, which bind as crate inputs.
fn verify_archives(
    archives: &[(PathBuf, bool)],
    generated: &BTreeSet<Vec<u8>>,
    upstream_rlibs: &[PathBuf],
    reported: &[PathBuf],
) -> Result<(), String> {
    let mut libraries = None;
    let mut rlibs = None;
    for (path, staticlib) in archives {
        let mut archive = match Archive::open(path) {
            Ok(archive) => archive,
            // Rustc skips an archive that the target cannot produce.
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("'{}': {error}", path.display())),
        };
        for member in archive.members.clone() {
            if generated.contains(&member.name) {
                continue;
            }
            let data = archive.read(&member).map_err(|error| error.to_string())?;
            let held = Sources::opened(&mut libraries, reported)?.hold(&member.name, &data)?
                || *staticlib && Sources::opened(&mut rlibs, upstream_rlibs)?.hold(&member.name, &data)?;
            if !held {
                return Err(format!(
                    "'{}' holds '{}', which no reported native library holds",
                    path.display(),
                    String::from_utf8_lossy(&member.name)
                ));
            }
        }
    }
    Ok(())
}

/// Archives indexed by member name.
struct Sources {
    archives: Vec<Archive>,
    members: BTreeMap<Vec<u8>, Vec<(usize, Member)>>,
}

impl Sources {
    /// Open `paths` the first time the sources are needed.
    fn opened<'a>(sources: &'a mut Option<Self>, paths: &[PathBuf]) -> Result<&'a mut Self, String> {
        let opened = match sources.take() {
            Some(opened) => opened,
            None => Self::open(paths)?,
        };
        Ok(sources.insert(opened))
    }

    fn open(paths: &[PathBuf]) -> Result<Self, String> {
        let mut sources = Self {
            archives: Vec::with_capacity(paths.len()),
            members: BTreeMap::new(),
        };
        for path in paths {
            let archive = Archive::open(path).map_err(|error| format!("'{}': {error}", path.display()))?;
            for member in &archive.members {
                sources
                    .members
                    .entry(member.name.clone())
                    .or_default()
                    .push((sources.archives.len(), member.clone()));
            }
            sources.archives.push(archive);
        }
        Ok(sources)
    }

    /// Whether a member of the same name holds the same bytes.
    fn hold(&mut self, name: &[u8], data: &[u8]) -> Result<bool, String> {
        for (index, member) in self.members.get(name).into_iter().flatten() {
            let candidate = self.archives[*index].read(member).map_err(|error| error.to_string())?;
            if same_member(&candidate, data) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Equal bytes, apart from the alignment padding that an archive writer may add after a member.
fn same_member(left: &[u8], right: &[u8]) -> bool {
    let (short, long) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    long.len() - short.len() < MAX_MEMBER_PADDING
        && long.starts_with(short)
        && long[short.len()..].iter().all(|byte| matches!(byte, 0 | b'\n'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Member {
    name: Vec<u8>,
    offset: u64,
    size: u64,
}

/// The file members of a GNU, BSD, or COFF archive; symbol tables and the name table are not members.
struct Archive {
    file: File,
    members: Vec<Member>,
}

impl Archive {
    fn open(path: &Path) -> std::io::Result<Self> {
        let invalid = |message: &str| std::io::Error::new(ErrorKind::InvalidData, message.to_owned());
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != b"!<arch>\n" {
            return Err(invalid("not a regular ar archive"));
        }
        let mut members = Vec::new();
        let mut names = Vec::new();
        let mut position = 8u64;
        while position < length {
            let mut header = [0u8; 60];
            file.seek(SeekFrom::Start(position))?;
            file.read_exact(&mut header)?;
            if &header[58..] != b"`\n" {
                return Err(invalid("archive member header is malformed"));
            }
            let size = decimal(&header[48..58]).ok_or_else(|| invalid("archive member size is malformed"))?;
            let mut offset = position + 60;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= length)
                .ok_or_else(|| invalid("archive member exceeds the archive"))?;
            position = end + end % 2;
            let field = trim_end(&header[..16], b' ');
            let name = if let Some(length) = field.strip_prefix(b"#1/") {
                // BSD: the name precedes the data, padded with NUL bytes.
                let length = decimal(length)
                    .filter(|length| *length <= size)
                    .ok_or_else(|| invalid("archive member name is malformed"))?;
                let mut name =
                    vec![0u8; usize::try_from(length).map_err(|_| invalid("archive member name is too long"))?];
                file.read_exact(&mut name)?;
                offset += length;
                trim_end(&name, 0).to_vec()
            } else if field == b"//" {
                // GNU and COFF: the table of long names.
                names = vec![0u8; usize::try_from(size).map_err(|_| invalid("archive name table is too long"))?];
                file.read_exact(&mut names)?;
                continue;
            } else if let Some(start) = field
                .strip_prefix(b"/")
                .filter(|rest| rest.first().is_some_and(u8::is_ascii_digit))
            {
                let start = decimal(start)
                    .and_then(|start| usize::try_from(start).ok())
                    .filter(|start| *start < names.len())
                    .ok_or_else(|| invalid("archive member name is not in the name table"))?;
                let name = &names[start..];
                let end = name
                    .iter()
                    .position(|byte| matches!(byte, b'\n' | 0))
                    .unwrap_or(name.len());
                trim_end(&name[..end], b'/').to_vec()
            } else if field.starts_with(b"/") {
                // A symbol table or another special member.
                continue;
            } else {
                field.strip_suffix(b"/").unwrap_or(field).to_vec()
            };
            if name.is_empty() {
                return Err(invalid("archive member has no name"));
            }
            if !name.starts_with(b"__.SYMDEF") {
                members.push(Member {
                    name,
                    offset,
                    size: end - offset,
                });
            }
        }
        Ok(Self { file, members })
    }

    fn read(&mut self, member: &Member) -> std::io::Result<Vec<u8>> {
        let size = usize::try_from(member.size)
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "archive member is too large"))?;
        let mut data = vec![0u8; size];
        self.file.seek(SeekFrom::Start(member.offset))?;
        self.file.read_exact(&mut data)?;
        Ok(data)
    }
}

fn decimal(field: &[u8]) -> Option<u64> {
    let digits = trim_end(field, b' ');
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn trim_end(bytes: &[u8], padding: u8) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != padding)
        .map_or(0, |index| index + 1);
    &bytes[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &[u8], size: usize) -> Vec<u8> {
        let mut header = format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", "", 0, 0, 0, 644, size).into_bytes();
        header[..name.len()].copy_from_slice(name);
        header
    }

    /// A GNU archive with a symbol table, a long-name table, a short name, and a long name.
    fn gnu(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut table = Vec::new();
        let mut archive = b"!<arch>\n".to_vec();
        let mut body = Vec::new();
        for (name, data) in members {
            let field = if name.len() < 16 {
                format!("{name}/").into_bytes()
            } else {
                let field = format!("/{}", table.len()).into_bytes();
                table.extend_from_slice(format!("{name}/\n").as_bytes());
                field
            };
            body.extend(header(&field, data.len()));
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(b'\n');
            }
        }
        archive.extend(header(b"/", 4));
        archive.extend_from_slice(&[0, 0, 0, 0]);
        if !table.is_empty() {
            archive.extend(header(b"//", table.len()));
            archive.extend_from_slice(&table);
            if table.len() % 2 == 1 {
                archive.push(b'\n');
            }
        }
        archive.extend(body);
        archive
    }

    /// A BSD archive with a symbol table, names before the data, and members padded to eight bytes.
    fn bsd(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = b"!<arch>\n".to_vec();
        for (name, data) in [("__.SYMDEF SORTED", &[0u8; 8][..])].iter().chain(members) {
            let mut stored = name.as_bytes().to_vec();
            stored.resize(stored.len().next_multiple_of(8), 0);
            stored.extend_from_slice(data);
            stored.resize(stored.len().next_multiple_of(8), b'\n');
            archive.extend(header(
                format!("#1/{}", name.len().next_multiple_of(8)).as_bytes(),
                stored.len(),
            ));
            archive.extend(stored);
        }
        archive
    }

    #[test]
    fn archive_members_resolve_gnu_and_bsd_names() {
        let directory = tempfile::tempdir().unwrap();
        let long = "a-member-name-longer-than-sixteen-bytes.o";
        for (format, bytes) in [
            ("gnu", gnu(&[("short.o", b"odd"), (long, b"even")])),
            ("bsd", bsd(&[("short.o", b"odd"), (long, b"even")])),
        ] {
            let path = directory.path().join(format!("lib{format}.a"));
            std::fs::write(&path, bytes).unwrap();
            let mut archive = Archive::open(&path).unwrap();
            let names = archive
                .members
                .iter()
                .map(|member| member.name.clone())
                .collect::<Vec<_>>();
            assert_eq!(names, [b"short.o".to_vec(), long.as_bytes().to_vec()], "{format}");
            let data = archive.read(&archive.members[0].clone()).unwrap();
            assert!(same_member(&data, b"odd"), "{format}: {data:?}");
        }
        let thin = directory.path().join("libthin.a");
        std::fs::write(&thin, b"!<thin>\n").unwrap();
        assert!(
            Archive::open(&thin).is_err(),
            "a thin archive's members are other files"
        );
    }

    /// A future rustc that bundles a library the observation does not report must fail the check.
    #[test]
    fn a_bundled_member_from_an_unreported_library_fails_the_check() {
        let directory = tempfile::tempdir().unwrap();
        let path = |name: &str| directory.path().join(name);
        let generated = ["lib.rmeta", "consumer.consumer.0-cgu.0.rcgu.o"]
            .map(|name| name.as_bytes().to_vec())
            .into_iter()
            .collect::<BTreeSet<_>>();
        std::fs::write(path("libbar.a"), gnu(&[("bar.o", b"bar object")])).unwrap();
        std::fs::write(path("libbaz.a"), bsd(&[("baz.o", b"baz object")])).unwrap();
        // Rustc's writer pads the copied member, and the copy keeps its name.
        std::fs::write(
            path("libconsumer.rlib"),
            bsd(&[
                ("lib.rmeta", b"metadata"),
                ("consumer.consumer.0-cgu.0.rcgu.o", b"rust object"),
                ("bar.o", b"bar object"),
            ]),
        )
        .unwrap();
        let rlib = [(path("libconsumer.rlib"), false)];
        verify_archives(&rlib, &generated, &[], &[path("libbar.a")]).expect("the bundled library was reported");
        let error = verify_archives(&rlib, &generated, &[], &[path("libbaz.a")]).unwrap_err();
        assert!(
            error.contains("holds 'bar.o', which no reported native library holds"),
            "{error}"
        );
        verify_archives(&rlib, &generated, &[], &[]).expect_err("no library was reported");

        // Equal names with other bytes are another library.
        std::fs::write(path("libbar.a"), gnu(&[("bar.o", b"bar objecT")])).unwrap();
        verify_archives(&rlib, &generated, &[], &[path("libbar.a")]).expect_err("changed member bytes");

        // A staticlib also holds the members of its upstream rlibs, and only a staticlib does.
        std::fs::write(path("libupstream.rlib"), gnu(&[("bar.o", b"bar object")])).unwrap();
        verify_archives(&rlib, &generated, &[path("libupstream.rlib")], &[]).expect_err("an rlib");
        let staticlib = [(path("libconsumer.rlib"), true)];
        verify_archives(&staticlib, &generated, &[path("libupstream.rlib")], &[]).expect("a staticlib");
    }
}
