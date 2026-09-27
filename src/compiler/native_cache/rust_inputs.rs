//! Compiler-selected Rust libraries and the filename searches that can select them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::compiler::native_input_protocol::{NativeAssemblyObservation, NativeCratePattern, NativeInputObservation};
use crate::compiler::observation::RawCompilerInvocation;
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

use super::{
    NATIVE_CAPTURE_LIMITS, NativeCaptureBudget, NativeMetadataGuard, capture_guarded_file,
    compiler_library_search_values, compiler_rust_search_path, native_metadata_guard, native_relative_path,
    resolve_portable_compiler_path, semantic_mode, validate_sha256,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "root", content = "path", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum RustInputPath {
    Repository(String),
    OutputDirectory(String),
    HostToolchain(String),
    TargetLibrary(String),
}

impl RustInputPath {
    fn relative(&self) -> &str {
        match self {
            Self::Repository(path)
            | Self::OutputDirectory(path)
            | Self::HostToolchain(path)
            | Self::TargetLibrary(path) => path,
        }
    }

    fn with_relative(&self, relative: String) -> Self {
        match self {
            Self::Repository(_) => Self::Repository(relative),
            Self::OutputDirectory(_) => Self::OutputDirectory(relative),
            Self::HostToolchain(_) => Self::HostToolchain(relative),
            Self::TargetLibrary(_) => Self::TargetLibrary(relative),
        }
    }

    /// Split a validated path into its directory and entry name without touching the filesystem.
    fn split_file(&self) -> RailResult<(Self, &str)> {
        self.validate()?;
        let relative = Path::new(self.relative());
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| RailError::message("Rust input path names no directory entry"))?;
        let parent = native_relative_path(relative.parent().unwrap_or_else(|| Path::new("")))?;
        Ok((self.with_relative(parent), name))
    }

    fn child(&self, name: &str) -> Self {
        let relative = self.relative();
        self.with_relative(if relative.is_empty() {
            name.to_string()
        } else {
            format!("{relative}/{name}")
        })
    }

    fn validate(&self) -> RailResult<()> {
        let path = self.relative();
        if path.len() > super::MAX_DYNAMIC_REPOSITORY_PATH_BYTES
            || path.contains(['\0', '\\'])
            || Path::new(path)
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(RailError::message("Rust input path is not a bounded relative path"));
        }
        if !matches!(self, Self::Repository(_)) && Path::new(path).components().count() > 1 {
            return Err(RailError::message(
                "Rust toolchain input is outside its captured library directory",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RustCrateSelector {
    pub(super) name: String,
    pub(super) selected: Vec<RustInputPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RustSearchSelector {
    pub(super) directory: RustInputPath,
    pub(super) patterns: Vec<NativeCratePattern>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RustInputSelector {
    pub(super) crates: Vec<RustCrateSelector>,
    pub(super) searches: Vec<RustSearchSelector>,
}

impl RustInputSelector {
    pub(super) fn validate(&self) -> RailResult<()> {
        if self.crates.len() > 4096
            || self.searches.len() > 512
            || self.crates.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .searches
                .windows(2)
                .any(|pair| pair[0].directory >= pair[1].directory)
            || self.crates.iter().any(|source| {
                source.name.is_empty()
                    || source.name.len() > 256
                    || !source
                        .name
                        .chars()
                        .all(|character| character == '_' || character.is_alphanumeric())
                    || source.selected.is_empty()
                    || source.selected.len() > 4
                    || source.selected.windows(2).any(|pair| pair[0] >= pair[1])
                    || source
                        .selected
                        .iter()
                        .any(|path| path.relative().is_empty() || path.validate().is_err())
            })
            || self.searches.iter().any(|search| {
                search.directory.validate().is_err()
                    || search.patterns.is_empty()
                    || search.patterns.len() > 4096 * 10
                    || search.patterns.windows(2).any(|pair| pair[0] >= pair[1])
                    || search.patterns.iter().any(|pattern| pattern.validate().is_err())
            })
            || serde_json::to_vec(self)?.len() > super::MAX_SOURCE_PATH_BYTES
        {
            return Err(RailError::message(
                "Rust input selector is invalid or exceeds its bounds",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum RustInputContent {
    Repository {
        content_digest: String,
        bytes: u64,
        mode: u32,
    },
    Toolchain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RustInputFile {
    pub(super) path: RustInputPath,
    pub(super) content: RustInputContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RustInputSearch {
    pub(super) directory: RustInputPath,
    pub(super) present: bool,
    pub(super) candidates: Vec<RustInputPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RustInputWitness {
    pub(super) selector: RustInputSelector,
    pub(super) files: Vec<RustInputFile>,
    pub(super) searches: Vec<RustInputSearch>,
}

impl RustInputWitness {
    pub(super) fn selector(&self) -> &RustInputSelector {
        &self.selector
    }

    pub(super) fn validate(&self) -> RailResult<()> {
        self.selector.validate()?;
        if self.files.len() > super::MAX_SOURCE_ENTRIES
            || self.files.windows(2).any(|pair| pair[0].path >= pair[1].path)
            || self.searches.len() != self.selector.searches.len()
        {
            return Err(RailError::message(
                "Rust input witness is not a bounded canonical closure",
            ));
        }
        let mut expected_files = self
            .selector
            .crates
            .iter()
            .flat_map(|source| source.selected.iter())
            .collect::<BTreeSet<_>>();
        for (search, selector) in self.searches.iter().zip(&self.selector.searches) {
            if search.directory != selector.directory
                || (!search.present && !search.candidates.is_empty())
                || search.candidates.windows(2).any(|pair| pair[0] >= pair[1])
                || search.candidates.iter().any(|path| {
                    path.validate().is_err()
                        || !same_root(path, &search.directory)
                        || Path::new(path.relative()).parent() != Some(Path::new(search.directory.relative()))
                        || !Path::new(path.relative())
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| selector.patterns.iter().any(|pattern| pattern.matches(name)))
                })
            {
                return Err(RailError::message(
                    "Rust input search witness does not match its selector",
                ));
            }
            expected_files.extend(&search.candidates);
        }
        if expected_files.into_iter().ne(self.files.iter().map(|file| &file.path)) {
            return Err(RailError::message("Rust input witness omits or adds selected files"));
        }
        for file in &self.files {
            file.path.validate()?;
            match (&file.path, &file.content) {
                (
                    RustInputPath::Repository(_) | RustInputPath::OutputDirectory(_),
                    RustInputContent::Repository {
                        content_digest, bytes, ..
                    },
                ) => {
                    validate_sha256(content_digest)?;
                    if *bytes > super::MAX_SOURCE_BYTES {
                        return Err(RailError::message("Rust input file exceeds its byte bound"));
                    }
                }
                (RustInputPath::HostToolchain(_) | RustInputPath::TargetLibrary(_), RustInputContent::Toolchain) => {}
                _ => return Err(RailError::message("Rust input content uses the wrong authority")),
            }
        }
        Ok(())
    }
}

fn same_root(left: &RustInputPath, right: &RustInputPath) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

#[derive(Debug, Clone)]
struct Roots {
    repository: PathBuf,
    repository_spellings: BTreeSet<PathBuf>,
    host: PathBuf,
    host_spelling: PathBuf,
    target: Option<PathBuf>,
    target_spelling: Option<PathBuf>,
    output: Option<PathBuf>,
    output_spelling: Option<PathBuf>,
}

impl Roots {
    fn capture(repository: &Path, host: &Path, target: Option<&Path>) -> RailResult<Self> {
        let canonical_repository = crate::utils::canonicalize_existing(repository)?;
        Ok(Self {
            repository_spellings: BTreeSet::from([repository.to_path_buf(), canonical_repository.clone()]),
            repository: canonical_repository,
            host: crate::utils::canonicalize_allow_missing(host)?,
            host_spelling: host.into(),
            target: target.map(crate::utils::canonicalize_allow_missing).transpose()?,
            target_spelling: target.map(Path::to_path_buf),
            output: None,
            output_spelling: None,
        })
    }

    fn capture_repository_spelling(&mut self, path: &Path) -> RailResult<()> {
        if self.repository_spellings.iter().any(|root| path.starts_with(root)) {
            return Ok(());
        }
        let canonical = crate::utils::canonicalize_allow_missing(path)?;
        let Ok(relative) = canonical.strip_prefix(&self.repository) else {
            return Ok(());
        };
        let mut spelling = path.to_path_buf();
        for _ in relative.components() {
            if !spelling.pop() {
                return Err(RailError::message("Rust input root spelling is unavailable"));
            }
        }
        // Only a different spelling of the named root is admissible. The
        // relative input path must stay identical, excluding internal aliases.
        let parent = spelling
            .parent()
            .ok_or_else(|| RailError::message("Rust input root spelling has no parent"))?;
        if spelling.join(relative) != path
            || crate::utils::canonicalize_existing(&spelling)? != self.repository
            || crate::utils::canonicalize_existing(parent)?.starts_with(&self.repository)
        {
            return Err(RailError::message(
                "Rust input does not preserve its repository-relative path",
            ));
        }
        self.repository_spellings.insert(spelling);
        if self.repository_spellings.len() > 512 {
            return Err(RailError::message("Rust input root spelling bound exceeded"));
        }
        Ok(())
    }

    fn encode(&self, path: &Path) -> RailResult<RustInputPath> {
        let canonical = crate::utils::canonicalize_allow_missing(path)?;
        let mapped = self
            .target_spelling
            .as_ref()
            .zip(self.target.as_ref())
            .into_iter()
            .chain(std::iter::once((&self.host_spelling, &self.host)))
            .chain(self.output_spelling.as_ref().zip(self.output.as_ref()))
            .chain(
                self.repository_spellings
                    .iter()
                    .map(|spelling| (spelling, &self.repository)),
            )
            .find_map(|(spelling, root)| path.strip_prefix(spelling).ok().map(|relative| root.join(relative)))
            .unwrap_or_else(|| path.to_path_buf());
        if canonical != mapped {
            return Err(RailError::message(
                "Rust input crosses a symlink or noncanonical path boundary",
            ));
        }
        let path = canonical.as_path();
        let encoded = if let Some(relative) = self.target.as_ref().and_then(|root| path.strip_prefix(root).ok()) {
            RustInputPath::TargetLibrary(native_relative_path(relative)?)
        } else if let Ok(relative) = path.strip_prefix(&self.host) {
            RustInputPath::HostToolchain(native_relative_path(relative)?)
        } else if let Some(relative) = self.output.as_ref().and_then(|root| path.strip_prefix(root).ok()) {
            RustInputPath::OutputDirectory(native_relative_path(relative)?)
        } else if let Ok(relative) = path.strip_prefix(&self.repository) {
            RustInputPath::Repository(native_relative_path(relative)?)
        } else {
            return Err(RailError::message(
                "Rust input is outside the repository and captured toolchain libraries",
            ));
        };
        encoded.validate()?;
        Ok(encoded)
    }

    fn resolve(&self, path: &RustInputPath) -> RailResult<PathBuf> {
        path.validate()?;
        let root = match path {
            RustInputPath::Repository(_) => &self.repository,
            RustInputPath::OutputDirectory(_) => self
                .output
                .as_ref()
                .ok_or_else(|| RailError::message("Rust output directory authority is unavailable"))?,
            RustInputPath::HostToolchain(_) => &self.host,
            RustInputPath::TargetLibrary(_) => self
                .target
                .as_ref()
                .ok_or_else(|| RailError::message("Rust target library authority is unavailable"))?,
        };
        let resolved = root.join(path.relative());
        if self.encode(&resolved)? != *path {
            return Err(RailError::message("Rust input path authority changed"));
        }
        Ok(resolved)
    }

    fn revalidate(&self) -> RailResult<()> {
        for (spelling, expected) in self
            .target_spelling
            .as_ref()
            .zip(self.target.as_ref())
            .into_iter()
            .chain(std::iter::once((&self.host_spelling, &self.host)))
            .chain(self.output_spelling.as_ref().zip(self.output.as_ref()))
            .chain(
                self.repository_spellings
                    .iter()
                    .map(|spelling| (spelling, &self.repository)),
            )
        {
            if crate::utils::canonicalize_allow_missing(spelling)? != *expected {
                return Err(RailError::message(
                    "Rust input root spelling changed its captured directory",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct DirectorySnapshot {
    present: bool,
    files: BTreeMap<String, Option<NativeMetadataGuard>>,
}

/// Rustc's filename queries for one directory, indexed by prefix.
///
/// A dependency directory holds thousands of entries and a large crate searches it with thousands of
/// queries. Matching each entry against the prefixes it actually starts with keeps a listing linear in
/// its entries instead of proportional to entries times queries.
struct PatternIndex<'a> {
    suffixes: std::collections::HashMap<&'a str, Vec<&'a str>>,
    prefix_lengths: Vec<usize>,
}

impl<'a> PatternIndex<'a> {
    fn new(patterns: impl IntoIterator<Item = &'a NativeCratePattern>) -> Self {
        let mut suffixes = std::collections::HashMap::<&str, Vec<&str>>::new();
        for pattern in patterns {
            suffixes.entry(&pattern.prefix).or_default().push(&pattern.suffix);
        }
        let mut prefix_lengths = suffixes.keys().map(|prefix| prefix.len()).collect::<Vec<_>>();
        prefix_lengths.sort_unstable();
        prefix_lengths.dedup();
        Self {
            suffixes,
            prefix_lengths,
        }
    }

    /// Exactly `NativeCratePattern::matches` for any of the indexed queries.
    fn matches(&self, name: &str) -> bool {
        self.prefix_lengths
            .iter()
            .take_while(|length| **length <= name.len())
            .filter_map(|length| name.get(..*length).map(|prefix| (*length, prefix)))
            .any(|(length, prefix)| {
                self.suffixes.get(prefix).is_some_and(|suffixes| {
                    suffixes
                        .iter()
                        .any(|suffix| name.len() >= length + suffix.len() && name.ends_with(suffix))
                })
            })
    }
}

/// Resolve each captured directory once, then name its entries lexically.
///
/// A directory resolution proves the directory is canonical within its root. Callers still read each
/// entry's own metadata and reject a symbolic link, so an entry needs no separate canonicalization.
struct DirectoryResolver<'a> {
    roots: &'a Roots,
    resolved: BTreeMap<RustInputPath, PathBuf>,
}

impl<'a> DirectoryResolver<'a> {
    fn new(roots: &'a Roots) -> Self {
        Self {
            roots,
            resolved: BTreeMap::new(),
        }
    }

    fn directory(&mut self, directory: &RustInputPath) -> RailResult<PathBuf> {
        if let Some(physical) = self.resolved.get(directory) {
            return Ok(physical.clone());
        }
        let physical = self.roots.resolve(directory)?;
        self.resolved.insert(directory.clone(), physical.clone());
        Ok(physical)
    }

    /// The physical path, directory, and entry name of one file input.
    fn file<'p>(&mut self, path: &'p RustInputPath) -> RailResult<(PathBuf, RustInputPath, &'p str)> {
        let (directory, name) = path.split_file()?;
        Ok((self.directory(&directory)?.join(name), directory, name))
    }
}

/// All candidate generations before rustc knows which crate names it will load.
pub(crate) struct ColdRustInputGuard {
    roots: Roots,
    directories: BTreeMap<RustInputPath, DirectorySnapshot>,
    bindings: BTreeMap<PathBuf, DirectoryBinding>,
}

impl ColdRustInputGuard {
    pub(super) fn capture(
        observation: &RawCompilerInvocation,
        workspace_root: &Path,
        host_library_directory: &Path,
        target_library_directory: Option<&Path>,
    ) -> RailResult<Self> {
        Self::capture_selected(
            observation,
            workspace_root,
            host_library_directory,
            target_library_directory,
            None,
        )
    }

    fn capture_selected(
        observation: &RawCompilerInvocation,
        workspace_root: &Path,
        host_library_directory: &Path,
        target_library_directory: Option<&Path>,
        selector: Option<&RustInputSelector>,
    ) -> RailResult<Self> {
        let mut roots = Roots::capture(workspace_root, host_library_directory, target_library_directory)?;
        let current_directory = std::env::current_dir()?;
        roots.output_spelling =
            super::compiler_output_directory(&observation.compiler_arguments, &current_directory, workspace_root)?;
        roots.output = roots
            .output_spelling
            .as_deref()
            .map(crate::utils::canonicalize_allow_missing)
            .transpose()?;
        let mut search_paths = Vec::new();
        if selector.is_none_or(|selector| !selector.crates.is_empty() || !selector.searches.is_empty()) {
            for value in compiler_library_search_values(&observation.compiler_arguments)? {
                if let Some(path) = compiler_rust_search_path(value, "all")? {
                    let path = resolve_portable_compiler_path(path, &current_directory, workspace_root)?;
                    roots.capture_repository_spelling(&path)?;
                    search_paths.push(path);
                }
            }
            for (index, argument) in observation.compiler_arguments.iter().enumerate() {
                let value = if argument == "--extern" {
                    observation.compiler_arguments.get(index + 1).map(String::as_str)
                } else {
                    argument.strip_prefix("--extern=")
                };
                if let Some((_, path)) = value.and_then(|value| value.split_once('=')) {
                    roots.capture_repository_spelling(&resolve_portable_compiler_path(
                        path,
                        &current_directory,
                        workspace_root,
                    )?)?;
                }
            }
        }
        let mut directories = BTreeSet::new();
        if let Some(selector) = selector {
            directories.extend(selector.searches.iter().map(|search| search.directory.clone()));
            for selected in selector.crates.iter().flat_map(|source| &source.selected) {
                directories.insert(selected.split_file()?.0);
            }
        } else {
            for path in search_paths {
                directories.insert(roots.encode(&path)?);
            }
            directories.insert(roots.encode(&roots.host)?);
            if let Some(target) = &roots.target {
                directories.insert(roots.encode(target)?);
            }
            for (_, file) in &observation.dependency_artifacts {
                let selected = roots.encode(&file.path.resolve(workspace_root))?;
                let path = roots.resolve(&selected)?;
                directories.insert(
                    roots.encode(
                        path.parent()
                            .ok_or_else(|| RailError::message("Rust extern input has no parent"))?,
                    )?,
                );
            }
        }
        if directories.len() > 512 {
            return Err(RailError::message("Rust input directory bound exceeded"));
        }
        let bindings = capture_bindings(&roots, &directories)?;
        let started = Instant::now();
        let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
        let directories = directories
            .into_iter()
            .map(|directory| {
                let patterns = selector.map(|selector| {
                    PatternIndex::new(
                        selector
                            .searches
                            .iter()
                            .filter(|search| search.directory == directory)
                            .flat_map(|search| search.patterns.iter()),
                    )
                });
                let selected = selector.map(|selector| {
                    selector
                        .crates
                        .iter()
                        .flat_map(|source| &source.selected)
                        .filter_map(|path| {
                            (same_root(path, &directory)
                                && Path::new(path.relative()).parent() == Some(Path::new(directory.relative())))
                            .then(|| Path::new(path.relative()).file_name().and_then(|name| name.to_str()))
                            .flatten()
                        })
                        .collect::<BTreeSet<_>>()
                });
                let snapshot = snapshot_directory(
                    &roots.resolve(&directory)?,
                    patterns.as_ref(),
                    selected.as_ref(),
                    started,
                    &mut budget,
                )?;
                Ok((directory, snapshot))
            })
            .collect::<RailResult<_>>()?;
        revalidate_bindings(&bindings)?;
        Ok(Self {
            roots,
            directories,
            bindings,
        })
    }

    pub(super) fn complete(self, observation: &NativeInputObservation) -> RailResult<RustInputCapture> {
        match observation.assembly {
            NativeAssemblyObservation::NoCodegen | NativeAssemblyObservation::Absent => {}
            NativeAssemblyObservation::Present => {
                return Err(RailError::message("compiler assembly input reads are not observed"));
            }
            NativeAssemblyObservation::ImportedLtoUnobserved => {
                return Err(RailError::message(
                    "compiler imported LTO assembly inputs are not observed",
                ));
            }
        }
        let mut crates = observation
            .crates
            .iter()
            .map(|source| {
                let mut selected = source
                    .files
                    .iter()
                    .map(|file| self.roots.encode(Path::new(file)))
                    .collect::<RailResult<Vec<_>>>()?;
                selected.sort_unstable();
                selected.dedup();
                Ok(RustCrateSelector {
                    name: source.name.clone(),
                    selected,
                })
            })
            .collect::<RailResult<Vec<_>>>()?;
        crates.sort_unstable();
        crates.dedup();
        let mut searches = observation
            .searches
            .iter()
            .map(|search| {
                Ok(RustSearchSelector {
                    directory: self.roots.encode(Path::new(&search.directory))?,
                    patterns: search.patterns.clone(),
                })
            })
            .collect::<RailResult<Vec<_>>>()?;
        searches.sort_unstable();
        let selector = RustInputSelector { crates, searches };
        selector.validate()?;
        for search in &observation.searches {
            let directory = self.roots.encode(Path::new(&search.directory))?;
            let before = self
                .directories
                .get(&directory)
                .ok_or_else(|| RailError::message("compiler searched an uncaptured Rust library directory"))?;
            let patterns = PatternIndex::new(&search.patterns);
            let names = before
                .files
                .keys()
                .filter(|name| patterns.matches(name))
                .cloned()
                .collect::<Vec<_>>();
            if names != search.files {
                return Err(RailError::message(
                    "Rust library search changed before compiler selection",
                ));
            }
        }
        self.finish(selector)
    }

    fn finish(mut self, selector: RustInputSelector) -> RailResult<RustInputCapture> {
        let started = Instant::now();
        let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
        let mut paths = selector
            .crates
            .iter()
            .flat_map(|source| source.selected.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut searches = Vec::new();
        let mut resolver = DirectoryResolver::new(&self.roots);
        for search in &selector.searches {
            let before = self
                .directories
                .get(&search.directory)
                .ok_or_else(|| RailError::message("Rust search selector names an uncaptured directory"))?;
            let patterns = PatternIndex::new(&search.patterns);
            let current = snapshot_directory(
                &resolver.directory(&search.directory)?,
                Some(&patterns),
                None,
                started,
                &mut budget,
            )?;
            let expected = before
                .files
                .keys()
                .filter(|name| patterns.matches(name))
                .collect::<Vec<_>>();
            if current.present != before.present || current.files.keys().ne(expected.iter().copied()) {
                let changed = current.files.keys().find(|name| !expected.contains(name)).or_else(|| {
                    expected
                        .iter()
                        .find(|name| !current.files.contains_key(name.as_str()))
                        .copied()
                });
                return Err(RailError::message(format!(
                    "Rust library search candidates changed during compilation in '{}': {}",
                    resolver.directory(&search.directory)?.display(),
                    changed.map_or("directory presence changed", String::as_str),
                )));
            }
            let candidates = current
                .files
                .keys()
                .map(|name| search.directory.child(name))
                .collect::<Vec<_>>();
            paths.extend(candidates.iter().cloned());
            searches.push(RustInputSearch {
                directory: search.directory.clone(),
                present: before.present,
                candidates,
            });
        }
        // Empty matching searches are proved by rustc's retained filename index
        // and the snapshots above. Only directories with matching or selected
        // files need generation guards against a same-name directory swap.
        let file_directories = paths
            .iter()
            .map(|path| path.split_file().map(|(directory, _)| directory))
            .collect::<RailResult<BTreeSet<_>>>()?;
        let required_bindings = binding_paths(&self.roots, &file_directories)?;
        if required_bindings.keys().any(|path| !self.bindings.contains_key(path)) {
            return Err(RailError::message(
                "Rust library binding was not captured before compilation",
            ));
        }
        self.bindings.retain(|path, _| required_bindings.contains_key(path));
        let mut files = Vec::new();
        let mut guards = BTreeMap::new();
        for path in paths {
            let (physical, parent, name) = resolver.file(&path)?;
            let expected = self
                .directories
                .get(&parent)
                .and_then(|directory| directory.files.get(name))
                .and_then(Option::as_ref)
                .ok_or_else(|| RailError::message("selected Rust library was not a regular pre-execution input"))?;
            let metadata = fs::symlink_metadata(&physical)?;
            if !metadata.is_file()
                || crate::utils::is_symlink_or_reparse(&metadata)
                || native_metadata_guard(&physical, &metadata)? != *expected
            {
                return Err(RailError::message("Rust library input changed during compilation"));
            }
            let content = if matches!(path, RustInputPath::Repository(_) | RustInputPath::OutputDirectory(_)) {
                let (content_digest, guard, bytes) = capture_guarded_file(&physical, started, &mut budget)?;
                if guard != *expected {
                    return Err(RailError::message(
                        "Rust library input changed before its bytes were captured",
                    ));
                }
                RustInputContent::Repository {
                    content_digest,
                    bytes,
                    mode: semantic_mode(&metadata),
                }
            } else {
                RustInputContent::Toolchain
            };
            guards.insert(path.clone(), expected.clone());
            files.push(RustInputFile { path, content });
        }
        let capture = RustInputCapture {
            roots: self.roots,
            witness: RustInputWitness {
                selector,
                files,
                searches,
            },
            guards,
            bindings: self.bindings,
            bytes_hashed: budget.bytes_hashed,
        };
        capture.witness.validate()?;
        capture.revalidate()?;
        Ok(capture)
    }
}

#[derive(Debug, Clone)]
pub(super) struct RustInputCapture {
    roots: Roots,
    witness: RustInputWitness,
    guards: BTreeMap<RustInputPath, NativeMetadataGuard>,
    bindings: BTreeMap<PathBuf, DirectoryBinding>,
    bytes_hashed: u64,
}

impl RustInputCapture {
    pub(super) fn capture(
        selector: &RustInputSelector,
        observation: &RawCompilerInvocation,
        workspace_root: &Path,
        host_library_directory: &Path,
        target_library_directory: Option<&Path>,
    ) -> RailResult<Self> {
        selector.validate()?;
        ColdRustInputGuard::capture_selected(
            observation,
            workspace_root,
            host_library_directory,
            target_library_directory,
            Some(selector),
        )?
        .finish(selector.clone())
    }

    pub(super) fn witness(&self) -> &RustInputWitness {
        &self.witness
    }
    pub(super) fn bytes_hashed(&self) -> u64 {
        self.bytes_hashed
    }
    pub(super) fn guard_identity(&self) -> RailResult<String> {
        Ok(ContentDigest::sha256(&serde_json::to_vec(&(
            self.guards.iter().collect::<Vec<_>>(),
            &self.bindings,
        ))?)
        .to_string())
    }

    pub(super) fn revalidate(&self) -> RailResult<()> {
        self.roots.revalidate()?;
        revalidate_bindings(&self.bindings)?;
        let started = Instant::now();
        let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
        let mut resolver = DirectoryResolver::new(&self.roots);
        for (search, selector) in self.witness.searches.iter().zip(&self.witness.selector.searches) {
            let patterns = PatternIndex::new(&selector.patterns);
            let current = snapshot_directory(
                &resolver.directory(&search.directory)?,
                Some(&patterns),
                None,
                started,
                &mut budget,
            )?;
            if current.present != search.present
                || current
                    .files
                    .keys()
                    .map(String::as_str)
                    .ne(search.candidates.iter().map(|path| {
                        Path::new(path.relative())
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("")
                    }))
            {
                return Err(RailError::message(
                    "Rust library search candidates changed before publication",
                ));
            }
        }
        for (path, expected) in &self.guards {
            let (physical, _, _) = resolver.file(path)?;
            let metadata = fs::symlink_metadata(&physical)?;
            if !metadata.is_file()
                || crate::utils::is_symlink_or_reparse(&metadata)
                || native_metadata_guard(&physical, &metadata)? != *expected
            {
                return Err(RailError::message("Rust library input changed before publication"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
enum DirectoryBinding {
    Generation(NativeMetadataGuard),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Entry {
        device: u64,
        inode: u64,
        mode: u32,
        #[serde(skip)]
        watch: std::sync::Arc<DirectoryWatch>,
    },
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug)]
struct DirectoryWatch {
    #[cfg(target_os = "macos")]
    watcher: kqueue::Watcher,
    #[cfg(target_os = "linux")]
    descriptor: rustix::fd::OwnedFd,
    directory: fs::File,
    changed: std::sync::Mutex<bool>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl DirectoryWatch {
    fn new(path: &Path) -> RailResult<Self> {
        let directory = fs::File::open(path)?;
        #[cfg(target_os = "macos")]
        let watcher = {
            let mut watcher = kqueue::Watcher::new()?;
            watcher.add_file(
                &directory,
                kqueue::EventFilter::EVFILT_VNODE,
                kqueue::FilterFlag::NOTE_RENAME | kqueue::FilterFlag::NOTE_DELETE | kqueue::FilterFlag::NOTE_REVOKE,
            )?;
            watcher.watch()?;
            watcher
        };
        #[cfg(target_os = "linux")]
        let descriptor = {
            use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
            use std::os::fd::AsRawFd as _;
            let descriptor =
                inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).map_err(std::io::Error::from)?;
            inotify::add_watch(
                &descriptor,
                format!("/proc/self/fd/{}", directory.as_raw_fd()),
                WatchFlags::MOVE_SELF | WatchFlags::DELETE_SELF | WatchFlags::ONLYDIR,
            )
            .map_err(std::io::Error::from)?;
            descriptor
        };
        Ok(Self {
            #[cfg(target_os = "macos")]
            watcher,
            #[cfg(target_os = "linux")]
            descriptor,
            directory,
            changed: std::sync::Mutex::new(false),
        })
    }

    fn unchanged(&self) -> bool {
        let Ok(mut changed) = self.changed.lock() else {
            return false;
        };
        if *changed {
            return false;
        }
        #[cfg(target_os = "macos")]
        let unchanged = self.watcher.poll(None).is_none();
        #[cfg(target_os = "linux")]
        let unchanged = {
            let mut buffer = [std::mem::MaybeUninit::uninit(); 4096];
            let mut reader = rustix::fs::inotify::Reader::new(&self.descriptor, &mut buffer);
            matches!(reader.next(), Err(rustix::io::Errno::AGAIN))
        };
        if !unchanged {
            *changed = true;
        }
        unchanged
    }
}

fn capture_bindings(
    roots: &Roots,
    directories: &BTreeSet<RustInputPath>,
) -> RailResult<BTreeMap<PathBuf, DirectoryBinding>> {
    binding_paths(roots, directories)?
        .into_iter()
        .map(|(path, alias_parent)| {
            let metadata = fs::symlink_metadata(&path)?;
            let generation = native_metadata_guard(&path, &metadata)?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if !alias_parent {
                let watch = std::sync::Arc::new(DirectoryWatch::new(&path)?);
                let generation = native_metadata_guard(&path, &watch.directory.metadata()?)?;
                let current = native_metadata_guard(&path, &fs::symlink_metadata(&path)?)?;
                if (current.device, current.inode, current.mode)
                    != (generation.device, generation.inode, generation.mode)
                    || !watch.unchanged()
                {
                    return Err(RailError::message(
                        "Rust library directory changed while installing its watch",
                    ));
                }
                return Ok((
                    path,
                    DirectoryBinding::Entry {
                        device: generation.device,
                        inode: generation.inode,
                        mode: generation.mode,
                        watch,
                    },
                ));
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let _ = alias_parent;
            // A link replaced within one tick of the coarse change clock leaves its parent's generation unchanged,
            // so the binding waits until the parent's last change is settled and must survive the wait.
            crate::utils::settle_change_time(&metadata);
            if native_metadata_guard(&path, &fs::symlink_metadata(&path)?)? != generation {
                return Err(RailError::message(
                    "Rust library directory parent changed while its generation settled",
                ));
            }
            Ok((path, DirectoryBinding::Generation(generation)))
        })
        .collect()
}

fn binding_paths(roots: &Roots, directories: &BTreeSet<RustInputPath>) -> RailResult<BTreeMap<PathBuf, bool>> {
    let mut bindings = BTreeMap::new();
    if directories.is_empty() {
        return Ok(bindings);
    }
    // Retaining only the final canonical root misses an alias changed and
    // restored while rustc runs. Watch the parents of followed links as well.
    let mut pending = roots.repository_spellings.iter().cloned().collect::<Vec<_>>();
    pending.push(roots.host_spelling.clone());
    pending.extend(roots.target_spelling.iter().cloned());
    pending.extend(roots.output_spelling.iter().cloned());
    let mut selected_links = BTreeSet::new();
    while let Some(selected) = pending.pop() {
        for component in selected.ancestors() {
            let metadata = match fs::symlink_metadata(component) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !crate::utils::is_symlink_or_reparse(&metadata) || !selected_links.insert(component.to_path_buf()) {
                continue;
            }
            if selected_links.len() > 40 {
                return Err(RailError::message("Rust input root symlink chain limit exceeded"));
            }
            let parent = component
                .parent()
                .ok_or_else(|| RailError::message("Rust input root link has no parent"))?;
            let target = fs::read_link(component)?;
            pending.push(if target.is_absolute() {
                target
            } else {
                parent.join(target)
            });
            let parent = crate::utils::canonicalize_existing(parent)?;
            bindings.insert(parent, true);
        }
    }
    for directory in directories {
        let directory_path = roots.resolve(directory)?;
        let mut parent = if cfg!(any(target_os = "linux", target_os = "macos")) {
            Some(directory_path)
        } else {
            directory_path.parent().map(Path::to_path_buf)
        };
        let boundary = match directory {
            RustInputPath::Repository(_) => &roots.repository,
            RustInputPath::OutputDirectory(_) => {
                let output = roots
                    .output
                    .as_ref()
                    .ok_or_else(|| RailError::message("Rust output directory authority is unavailable"))?;
                if output.starts_with(&roots.repository) {
                    &roots.repository
                } else {
                    output
                        .parent()
                        .ok_or_else(|| RailError::message("Rust output directory has no parent"))?
                }
            }
            RustInputPath::HostToolchain(_) => &roots.host,
            RustInputPath::TargetLibrary(_) => roots
                .target
                .as_ref()
                .ok_or_else(|| RailError::message("Rust target library authority is unavailable"))?,
        };
        while let Some(path) = parent.filter(|path| path.starts_with(boundary)) {
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) => {
                    bindings.entry(path.clone()).or_insert(false);
                }
                Ok(_) => {
                    return Err(RailError::message(
                        "Rust input directory parent is not a real directory",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            parent = path.parent().map(Path::to_path_buf);
        }
    }
    Ok(bindings)
}

fn revalidate_bindings(bindings: &BTreeMap<PathBuf, DirectoryBinding>) -> RailResult<()> {
    for (path, expected) in bindings {
        let metadata = fs::symlink_metadata(path)?;
        let current = native_metadata_guard(path, &metadata)?;
        let unchanged = match expected {
            DirectoryBinding::Generation(generation) => current == *generation,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            DirectoryBinding::Entry {
                device,
                inode,
                mode,
                watch,
            } => current.device == *device && current.inode == *inode && current.mode == *mode && watch.unchanged(),
        };
        if !metadata.is_dir()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || crate::utils::canonicalize_existing(path)? != *path
            || !unchanged
        {
            return Err(RailError::message(format!(
                "Rust library directory parent changed during input observation: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn snapshot_directory(
    path: &Path,
    patterns: Option<&PatternIndex<'_>>,
    selected: Option<&BTreeSet<&str>>,
    started: Instant,
    budget: &mut NativeCaptureBudget,
) -> RailResult<DirectorySnapshot> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DirectorySnapshot {
                present: false,
                files: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || crate::utils::canonicalize_existing(path)? != path
    {
        return Err(RailError::message(
            "Rust library search is not a real canonical directory",
        ));
    }
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        budget.check(0, started.elapsed())?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        budget.account_entry(name)?;
        if patterns.is_some_and(|patterns| !patterns.matches(name))
            && !selected.is_some_and(|selected| selected.contains(name))
        {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            // Concurrent Cargo jobs remove temporary entries. A subsequently
            // selected or reappearing candidate still has to match this snapshot.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let guard = if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) {
            Some(native_metadata_guard(&entry.path(), &metadata)?)
        } else {
            None
        };
        files.insert(name.into(), guard);
    }
    Ok(DirectorySnapshot { present: true, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_index_matches_exactly_the_queries_it_indexes() {
        let pattern = |prefix: &str, suffix: &str| NativeCratePattern {
            prefix: prefix.into(),
            suffix: suffix.into(),
        };
        let patterns = [
            pattern("libserde", ".rlib"),
            pattern("libserde", ".rmeta"),
            pattern("libserde_derive-0f1e2d", ".dylib"),
            pattern("libaa", "aa.rlib"),
            pattern("libé", ".rlib"),
        ];
        let index = PatternIndex::new(&patterns);
        for name in [
            "libserde.rlib",
            "libserde-1234.rmeta",
            "libserde_derive-0f1e2d.dylib",
            "libserde_derive-0f1e2d.rlib",
            "libserde.rlib.d",
            "libser.rlib",
            "libaaa.rlib",
            "libaaaa.rlib",
            "libé-01.rlib",
            "libe.rlib",
            "",
        ] {
            assert_eq!(
                index.matches(name),
                patterns.iter().any(|pattern| pattern.matches(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn lexical_directory_split_names_the_resolved_entry() {
        let path = RustInputPath::Repository("target/debug/deps/libfixture-01.rlib".into());
        let (directory, name) = path.split_file().unwrap();
        assert_eq!(directory, RustInputPath::Repository("target/debug/deps".into()));
        assert_eq!(name, "libfixture-01.rlib");
        assert_eq!(directory.child(name), path);
        let output = RustInputPath::OutputDirectory("libfixture-01.rmeta".into());
        let (directory, name) = output.split_file().unwrap();
        assert_eq!(directory, RustInputPath::OutputDirectory(String::new()));
        assert_eq!(directory.child(name), output);
        let toolchain = RustInputPath::HostToolchain("libstd-01.rlib".into());
        let (directory, name) = toolchain.split_file().unwrap();
        assert_eq!(directory, RustInputPath::HostToolchain(String::new()));
        assert_eq!(directory.child(name), toolchain);
        RustInputPath::Repository("../escape.rlib".into())
            .split_file()
            .expect_err("a parent component is not a directory entry");
    }
    use crate::compiler::native_input_protocol::{NATIVE_INPUT_PROTOCOL_VERSION, NativeCrateSearch, NativeCrateSource};
    use std::ffi::OsStr;

    struct Fixture {
        _temporary: tempfile::TempDir,
        workspace: PathBuf,
        host: PathBuf,
        dependencies: PathBuf,
        raw: RawCompilerInvocation,
        observed: NativeInputObservation,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().canonicalize().unwrap();
            let workspace = root.join("workspace");
            let host = root.join("host");
            let dependencies = workspace.join("deps");
            let observations = root.join("observations");
            for directory in [&workspace, &host, &dependencies, &observations] {
                fs::create_dir(directory).unwrap();
            }
            fs::write(workspace.join("source.rs"), "pub fn value() {}\n").unwrap();
            fs::write(dependencies.join("libdependency-a.rlib"), b"AAAA").unwrap();
            fs::write(dependencies.join("libdependency-b.rlib"), b"BBBB").unwrap();
            fs::write(dependencies.join("libunrelated.rlib"), b"unrelated bytes").unwrap();
            let arguments = vec![
                workspace.join("source.rs").into_os_string(),
                "--crate-type=rlib".into(),
                "--crate-name=consumer".into(),
                "--emit=metadata".into(),
                "--out-dir".into(),
                dependencies.clone().into_os_string(),
                format!("-Ldependency={}", dependencies.display()).into(),
                "--extern".into(),
                format!("dependency={}", dependencies.join("libdependency-a.rlib").display()).into(),
            ];
            let recorder = crate::compiler::observation::begin_invocation_in(
                &observations,
                &workspace,
                &workspace,
                OsStr::new("rustc"),
                &arguments,
            )
            .unwrap();
            let raw = recorder.observation().clone();
            let observed = NativeInputObservation {
                version: NATIVE_INPUT_PROTOCOL_VERSION,
                request_identity: "1".repeat(64),
                crates: vec![NativeCrateSource {
                    name: "dependency".into(),
                    files: vec![dependencies.join("libdependency-a.rlib").to_str().unwrap().into()],
                }],
                searches: vec![NativeCrateSearch {
                    directory: dependencies.to_str().unwrap().into(),
                    patterns: vec![NativeCratePattern {
                        prefix: "libdependency".into(),
                        suffix: ".rlib".into(),
                    }],
                    files: vec!["libdependency-a.rlib".into(), "libdependency-b.rlib".into()],
                }],
                assembly: NativeAssemblyObservation::NoCodegen,
                codegen: crate::compiler::native_input_protocol::NativeCodegenObservation::NotRun,
                macros: None,
            };
            Self {
                _temporary: temporary,
                workspace,
                host,
                dependencies,
                raw,
                observed,
            }
        }

        fn cold(&self) -> ColdRustInputGuard {
            ColdRustInputGuard::capture(&self.raw, &self.workspace, &self.host, None).unwrap()
        }
    }

    #[test]
    fn external_output_search_relocates_and_rejects_new_matching_candidates() {
        let mut fixture = Fixture::new();
        let external = fixture.workspace.parent().unwrap().join("external-output");
        fs::create_dir(&external).unwrap();
        let output_index = fixture
            .raw
            .compiler_arguments
            .iter()
            .position(|argument| argument == "--out-dir")
            .unwrap()
            + 1;
        fixture.raw.compiler_arguments[output_index] = external.to_str().unwrap().into();
        fixture
            .raw
            .compiler_arguments
            .push(format!("-Ldependency={}", external.display()));
        fixture.observed.searches.push(NativeCrateSearch {
            directory: external.to_str().unwrap().into(),
            patterns: fixture.observed.searches[0].patterns.clone(),
            files: Vec::new(),
        });
        let captured = fixture.cold().complete(&fixture.observed).unwrap();
        assert!(captured.witness.searches.iter().any(|search| search.directory
            == RustInputPath::OutputDirectory(String::new())
            && search.present
            && search.candidates.is_empty()));
        let relocated = fixture.workspace.parent().unwrap().join("relocated-output");
        fs::create_dir(&relocated).unwrap();
        fixture.raw.compiler_arguments[output_index] = relocated.to_str().unwrap().into();
        *fixture.raw.compiler_arguments.last_mut().unwrap() = format!("-Ldependency={}", relocated.display());
        let warm = RustInputCapture::capture(
            captured.witness.selector(),
            &fixture.raw,
            &fixture.workspace,
            &fixture.host,
            None,
        )
        .unwrap();
        assert_eq!(warm.witness(), captured.witness());
        fs::write(relocated.join("libdependency-c.rlib"), b"CCCC").unwrap();
        assert!(
            warm.revalidate().is_err(),
            "a new candidate at the relocated output root must invalidate reuse"
        );
        let changed = RustInputCapture::capture(
            captured.witness.selector(),
            &fixture.raw,
            &fixture.workspace,
            &fixture.host,
            None,
        )
        .unwrap();
        assert_eq!(changed.bytes_hashed(), 12);
        fs::write(relocated.join("libdependency-c.rlib"), b"DDDD").unwrap();
        assert!(
            changed.revalidate().is_err(),
            "same-size candidate changes must invalidate reuse"
        );
    }

    #[test]
    fn missing_additional_search_retains_negative_candidate_authority() {
        let mut fixture = Fixture::new();
        let missing = fixture.workspace.join("host-deps");
        fixture
            .raw
            .compiler_arguments
            .push(format!("-Ldependency={}", missing.display()));
        fixture
            .observed
            .searches
            .push(crate::compiler::native_input_protocol::NativeCrateSearch {
                directory: missing.to_str().unwrap().into(),
                patterns: fixture.observed.searches[0].patterns.clone(),
                files: Vec::new(),
            });
        let captured = fixture.cold().complete(&fixture.observed).unwrap();
        assert_eq!(captured.bytes_hashed(), 8);
        assert!(captured.witness.searches.iter().any(|search| search.directory
            == RustInputPath::Repository("host-deps".into())
            && !search.present
            && search.candidates.is_empty()));
        fs::create_dir(&missing).unwrap();
        fs::write(missing.join("libdependency-c.rlib"), b"CCCC").unwrap();
        assert!(
            captured.revalidate().is_err(),
            "a new matching candidate must invalidate the captured absence"
        );
        let warm = RustInputCapture::capture(
            captured.witness.selector(),
            &fixture.raw,
            &fixture.workspace,
            &fixture.host,
            None,
        )
        .unwrap();
        assert_ne!(warm.witness(), captured.witness());
        assert_eq!(warm.bytes_hashed(), 12);
    }

    #[test]
    fn captures_matching_candidate_bytes_and_allows_own_unrelated_outputs() {
        let fixture = Fixture::new();
        let cold = fixture.cold();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        fs::write(fixture.workspace.join("unrelated-linked-output"), b"Cargo output").unwrap();
        fs::write(fixture.dependencies.join("consumer.rmeta"), b"ordinary output").unwrap();
        let capture = cold.complete(&fixture.observed).unwrap();
        assert_eq!(capture.bytes_hashed(), 8);
        assert_eq!(capture.witness.files.len(), 2);
        assert_eq!(
            capture.witness.files[0].path,
            RustInputPath::OutputDirectory("libdependency-a.rlib".into())
        );
        assert_eq!(
            capture.witness.files[1].path,
            RustInputPath::OutputDirectory("libdependency-b.rlib".into())
        );
        fs::write(fixture.dependencies.join("consumer.d"), b"dep-info").unwrap();
        capture.revalidate().unwrap();
    }

    #[test]
    fn unselected_matching_candidate_content_changes_the_warm_witness() {
        let fixture = Fixture::new();
        let capture = fixture.cold().complete(&fixture.observed).unwrap();
        fs::write(fixture.dependencies.join("libdependency-b.rlib"), b"CCCC").unwrap();
        assert!(
            capture
                .revalidate()
                .unwrap_err()
                .to_string()
                .contains("Rust library input changed")
        );
        let recaptured = RustInputCapture::capture(
            capture.witness.selector(),
            &fixture.raw,
            &fixture.workspace,
            &fixture.host,
            None,
        )
        .unwrap();
        assert_ne!(recaptured.witness(), capture.witness());
        assert_eq!(recaptured.bytes_hashed(), 8);
    }

    #[test]
    fn selected_file_same_bytes_after_mutation_cannot_authorize_compiler_outputs() {
        let fixture = Fixture::new();
        let cold = fixture.cold();
        let path = fixture.dependencies.join("libdependency-a.rlib");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"CCCC").unwrap();
        fs::write(&path, b"AAAA").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(
            cold.complete(&fixture.observed)
                .unwrap_err()
                .to_string()
                .contains("Rust library input changed")
        );
    }

    #[test]
    fn empty_matching_search_does_not_guard_unrelated_sibling_outputs() {
        let mut fixture = Fixture::new();
        let output_parent = fixture.workspace.join("target/debug");
        let output = output_parent.join("deps");
        fs::create_dir_all(&output).unwrap();
        fixture
            .raw
            .compiler_arguments
            .push(format!("-Ldependency={}", output.display()));
        fixture.observed.searches.push(NativeCrateSearch {
            directory: output.to_str().unwrap().into(),
            patterns: fixture.observed.searches[0].patterns.clone(),
            files: Vec::new(),
        });
        let cold = fixture.cold();
        fs::write(output_parent.join("unrelated-library"), b"Cargo output").unwrap();
        fs::write(output.join("libunrelated.rlib"), b"unrelated metadata").unwrap();
        let captured = cold.complete(&fixture.observed).unwrap();
        assert_eq!(captured.bytes_hashed(), 8);
        captured.revalidate().unwrap();
    }

    #[test]
    fn search_directory_swap_back_is_rejected_without_original_file_mutation() {
        let fixture = Fixture::new();
        let alternate = fixture.workspace.join("alternate");
        fs::create_dir(&alternate).unwrap();
        fs::write(alternate.join("libdependency-a.rlib"), b"CCCC").unwrap();
        fs::write(alternate.join("libdependency-b.rlib"), b"DDDD").unwrap();
        let cold = fixture.cold();
        let warm = fixture.cold().complete(&fixture.observed).unwrap();
        let cloned = warm.clone();
        let saved = fixture.workspace.join("saved");
        fs::rename(&fixture.dependencies, &saved).unwrap();
        fs::rename(&alternate, &fixture.dependencies).unwrap();
        fs::rename(&fixture.dependencies, &alternate).unwrap();
        fs::rename(&saved, &fixture.dependencies).unwrap();
        assert!(
            cold.complete(&fixture.observed)
                .unwrap_err()
                .to_string()
                .contains("directory parent changed")
        );
        for capture in [&warm, &cloned, &warm] {
            assert!(
                capture.revalidate().is_err(),
                "a consumed rename event must remain invalid"
            );
        }
    }

    #[test]
    fn compiler_search_snapshot_cannot_add_an_unobserved_candidate() {
        let fixture = Fixture::new();
        let cold = fixture.cold();
        let mut observed = fixture.observed;
        observed.searches[0].files.push("libdependency-transient.rlib".into());
        assert!(
            cold.complete(&observed)
                .unwrap_err()
                .to_string()
                .contains("search changed before compiler selection")
        );
    }

    #[test]
    fn empty_warm_selector_does_not_scan_unselected_library_directories() {
        let fixture = Fixture::new();
        fs::remove_dir_all(&fixture.dependencies).unwrap();
        let capture = RustInputCapture::capture(
            &RustInputSelector::default(),
            &fixture.raw,
            &fixture.workspace,
            &fixture.workspace,
            None,
        )
        .unwrap();
        assert_eq!(capture.bytes_hashed(), 0);
        assert_eq!(capture.witness.files, Vec::new());
        assert_eq!(capture.witness.searches, Vec::new());
    }

    #[cfg(unix)]
    #[test]
    fn compiler_argument_root_alias_is_captured_before_selection_and_revalidated() {
        let mut fixture = Fixture::new();
        let alias = fixture.workspace.parent().unwrap().join("argument-root-alias");
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        let original = fixture.workspace.to_str().unwrap();
        let spelling = alias.to_str().unwrap();
        let observation_directory = fixture.workspace.parent().unwrap().join("alias-observations");
        fs::create_dir(&observation_directory).unwrap();
        let arguments = vec![
            fixture.workspace.join("source.rs").into_os_string(),
            "--crate-type=rlib".into(),
            "--crate-name=consumer".into(),
            "--emit=metadata".into(),
            "--out-dir".into(),
            fixture.dependencies.clone().into_os_string(),
            format!("-Ldependency={}", alias.join("deps").display()).into(),
            "--extern".into(),
            format!("dependency={}", alias.join("deps/libdependency-a.rlib").display()).into(),
        ];
        fixture.raw = crate::compiler::observation::begin_invocation_in(
            &observation_directory,
            &fixture.workspace,
            &fixture.workspace,
            OsStr::new("rustc"),
            &arguments,
        )
        .unwrap()
        .observation()
        .clone();
        for source in &mut fixture.observed.crates {
            for path in &mut source.files {
                *path = path.replace(original, spelling);
            }
        }
        fixture.observed.searches[0].directory = alias.join("deps").to_str().unwrap().into();
        let capture = fixture.cold().complete(&fixture.observed).unwrap();
        assert_eq!(capture.bytes_hashed(), 8);
        assert_eq!(
            capture.witness.files[0].path,
            RustInputPath::OutputDirectory("libdependency-a.rlib".into())
        );
        let warm = RustInputCapture::capture(
            capture.witness.selector(),
            &fixture.raw,
            &fixture.workspace,
            &fixture.host,
            None,
        )
        .unwrap();
        assert_eq!(warm.witness(), capture.witness());
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&fixture.host, &alias).unwrap();
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        for capture in [&capture, &warm] {
            assert!(
                capture
                    .revalidate()
                    .unwrap_err()
                    .to_string()
                    .contains("directory parent changed")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn compiler_argument_internal_alias_cannot_be_recaptured_as_a_root_spelling() {
        let mut fixture = Fixture::new();
        let internal_alias = fixture.workspace.join("internal-alias");
        std::os::unix::fs::symlink(&fixture.workspace, &internal_alias).unwrap();
        fixture
            .raw
            .compiler_arguments
            .push(format!("-Ldependency={}", internal_alias.join("deps").display()));
        assert!(
            ColdRustInputGuard::capture(&fixture.raw, &fixture.workspace, &fixture.host, None)
                .err()
                .expect("internal alias must be rejected")
                .to_string()
                .contains("symlink or noncanonical")
        );
        let external_alias = fixture.workspace.parent().unwrap().join("outer-alias");
        std::os::unix::fs::symlink(&fixture.workspace, &external_alias).unwrap();
        fixture.raw.compiler_arguments.pop().unwrap();
        fixture.raw.compiler_arguments.push(format!(
            "-Ldependency={}",
            external_alias.join("internal-alias/deps").display()
        ));
        assert!(
            ColdRustInputGuard::capture(&fixture.raw, &fixture.workspace, &fixture.host, None)
                .err()
                .expect("an outer alias must not hide an internal symlink")
                .to_string()
                .contains("does not preserve its repository-relative path")
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_root_alias_same_target_after_retarget_is_rejected() {
        let fixture = Fixture::new();
        let alias = fixture.workspace.parent().unwrap().join("workspace-alias");
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        let cold = ColdRustInputGuard::capture(&fixture.raw, &alias, &fixture.host, None).unwrap();
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&fixture.host, &alias).unwrap();
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        assert!(
            cold.complete(&fixture.observed)
                .unwrap_err()
                .to_string()
                .contains("directory parent changed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_root_alias_is_supported_but_internal_symlinks_are_rejected() {
        let fixture = Fixture::new();
        let alias = fixture.workspace.parent().unwrap().join("workspace-alias");
        std::os::unix::fs::symlink(&fixture.workspace, &alias).unwrap();
        let roots = Roots::capture(&alias, &fixture.host, None).unwrap();
        assert_eq!(
            roots.encode(&alias.join("deps/libdependency-a.rlib")).unwrap(),
            RustInputPath::Repository("deps/libdependency-a.rlib".into())
        );
        std::os::unix::fs::symlink(&fixture.dependencies, fixture.workspace.join("internal-alias")).unwrap();
        assert!(
            roots
                .encode(&alias.join("internal-alias/libdependency-a.rlib"))
                .unwrap_err()
                .to_string()
                .contains("symlink or noncanonical")
        );
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&fixture.host, &alias).unwrap();
        assert!(
            roots
                .revalidate()
                .unwrap_err()
                .to_string()
                .contains("root spelling changed")
        );
    }
}
