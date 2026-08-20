//! Retained output-tree protocol shared by cache domains.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{RailError, RailResult};
use crate::source::{ContentDigest, RepositoryPath};

pub(crate) const OUTPUT_MANIFEST_VERSION: u32 = 1;
const IO_BUFFER_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_ENTRIES: usize = 1_000_000;

/// Exact declared output tree for one cached result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputManifest {
    pub(crate) version: u32,
    pub(crate) digest: String,
    pub(crate) entries: Vec<OutputEntry>,
    pub(crate) files: usize,
    pub(crate) directories: usize,
    pub(crate) symlinks: usize,
    pub(crate) bytes: u64,
}

impl OutputManifest {
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    pub(crate) fn validate_unchanged(&self, root: &Path) -> RailResult<()> {
        for entry in &self.entries {
            let relative = RepositoryPath::new(Path::new(&entry.path))?;
            let path = root.join(relative.as_path());
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                RailError::message(format!(
                    "declared compiler output '{}' disappeared before publication: {error}",
                    entry.path
                ))
            })?;
            let unchanged = match &entry.kind {
                OutputEntryKind::Directory { mode } => {
                    metadata.is_dir() && !metadata.file_type().is_symlink() && output_mode(&metadata) == *mode
                }
                OutputEntryKind::File { digest, mode, bytes } => {
                    if !metadata.is_file() || metadata.file_type().is_symlink() || output_mode(&metadata) != *mode {
                        false
                    } else {
                        let (current, current_bytes) = digest_file(&path)?;
                        current_bytes == *bytes && format!("sha256:{current}") == *digest
                    }
                }
                OutputEntryKind::Symlink { target } => {
                    metadata.file_type().is_symlink()
                        && fs::read_link(&path)
                            .ok()
                            .and_then(|current| current.to_str().map(str::to_string))
                            .is_some_and(|current| current == *target)
                }
            };
            if !unchanged {
                return Err(RailError::with_help(
                    format!("declared compiler output '{}' changed before publication", entry.path),
                    "retry after concurrent writes to the isolated execution root have stopped",
                ));
            }
        }
        Ok(())
    }
}

/// One path in a retained output tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct OutputEntry {
    pub(crate) path: String,
    #[serde(flatten)]
    pub(crate) kind: OutputEntryKind,
}

/// Retained output kind and its restoration metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OutputEntryKind {
    Directory { mode: u32 },
    File { digest: String, mode: u32, bytes: u64 },
    Symlink { target: String },
}

/// Build a retained output manifest from slots already authenticated while streaming.
pub(crate) fn manifest_from_verified_native_slots(slots: &[(&str, &str, u64, u32)]) -> RailResult<OutputManifest> {
    let mut entries = BTreeMap::new();
    let mut bytes = 0u64;
    for (path, digest, length, mode) in slots {
        let path = RepositoryPath::new(Path::new(path))?;
        let logical = path.as_str().to_string();
        if entries
            .insert(
                logical.clone(),
                OutputEntryKind::File {
                    digest: (*digest).to_string(),
                    mode: *mode,
                    bytes: *length,
                },
            )
            .is_some()
        {
            return Err(RailError::message("verified native pack contains duplicate slots"));
        }
        bytes = bytes
            .checked_add(*length)
            .ok_or_else(|| RailError::message("verified native pack byte count overflow"))?;
        let mut parent = path.as_path().parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            let logical = crate::utils::path_to_git_format(directory);
            match entries.entry(logical) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(OutputEntryKind::Directory { mode: 0o755 });
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if matches!(entry.get(), OutputEntryKind::Directory { mode: 0o755 }) => {}
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(RailError::message(
                        "verified native pack slot collides with an output parent",
                    ));
                }
            }
            parent = directory.parent();
        }
    }
    if entries.len() > MAX_OUTPUT_ENTRIES {
        return Err(RailError::message(
            "verified native pack exceeds the output-entry bound",
        ));
    }
    let entries = entries
        .into_iter()
        .map(|(path, kind)| OutputEntry { path, kind })
        .collect::<Vec<_>>();
    let directories = entries
        .iter()
        .filter(|entry| matches!(entry.kind, OutputEntryKind::Directory { .. }))
        .count();
    let digest = output_manifest_digest(&entries)?;
    Ok(OutputManifest {
        version: OUTPUT_MANIFEST_VERSION,
        digest,
        files: slots.len(),
        directories,
        symlinks: 0,
        entries,
        bytes,
    })
}

/// Capture regular-file compiler outputs for Local CAS tests.
#[cfg(test)]
pub(crate) fn capture_native_compiler_outputs(root: &Path, paths: &[std::path::PathBuf]) -> RailResult<OutputManifest> {
    let mut captured = Vec::with_capacity(paths.len());
    let mut unique = std::collections::BTreeSet::new();
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| RailError::message("native compiler cache staging output escapes its root"))?;
        let relative = RepositoryPath::new(relative)?;
        if !unique.insert(relative.as_str().to_string()) {
            return Err(RailError::message("native compiler cache staging paths are not unique"));
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(RailError::message(
                "native compiler cache staging output is not a regular file",
            ));
        }
        let (digest, bytes) = digest_file(path)?;
        captured.push((
            relative.as_str().to_string(),
            format!("sha256:{digest}"),
            bytes,
            output_mode(&metadata),
        ));
    }
    let slots = captured
        .iter()
        .map(|(path, digest, bytes, mode)| (path.as_str(), digest.as_str(), *bytes, *mode))
        .collect::<Vec<_>>();
    manifest_from_verified_native_slots(&slots)
}

pub(crate) fn output_manifest_digest(entries: &[OutputEntry]) -> RailResult<String> {
    let domain = b"cargo-rail-output-manifest\0";
    let mut hasher = Sha256::new();
    let mut input_bytes = domain.len();
    hasher.update(domain);
    frame_hash(
        &mut hasher,
        &mut input_bytes,
        b"version",
        &OUTPUT_MANIFEST_VERSION.to_le_bytes(),
    );
    for entry in entries {
        frame_hash(&mut hasher, &mut input_bytes, b"entry", &serde_json::to_vec(entry)?);
    }
    crate::instrumentation::record_hash(input_bytes);
    let digest = ContentDigest::from_sha256_bytes(hasher.finalize().into());
    Ok(format!("output-manifest-v{OUTPUT_MANIFEST_VERSION}-sha256-{digest}"))
}

fn frame_hash(hasher: &mut Sha256, input_bytes: &mut usize, tag: &[u8], value: &[u8]) {
    hasher.update((tag.len() as u64).to_le_bytes());
    hasher.update(tag);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
    *input_bytes = input_bytes
        .saturating_add(16)
        .saturating_add(tag.len())
        .saturating_add(value.len());
}

fn digest_file(path: &Path) -> RailResult<(ContentDigest, u64)> {
    let mut file = File::open(path)
        .map_err(|error| RailError::message(format!("failed to read compiler output '{}': {error}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0_u8; IO_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            RailError::message(format!("failed to read compiler output '{}': {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes.saturating_add(read as u64);
    }
    crate::instrumentation::record_hash(usize::try_from(bytes).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(bytes).unwrap_or(usize::MAX));
    Ok((ContentDigest::from_sha256_bytes(hasher.finalize().into()), bytes))
}

pub(crate) fn symlink_target_escapes(path: &RepositoryPath, target: &str) -> bool {
    let target = Path::new(target);
    if target.is_absolute() {
        return true;
    }
    let mut depth = path.as_path().parent().map_or(0usize, |parent| {
        parent
            .components()
            .filter(|component| matches!(component, Component::Normal(_)))
            .count()
    });
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

#[cfg(unix)]
pub(crate) fn output_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode() & 0o7777
}

#[cfg(windows)]
pub(crate) fn output_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.is_dir() { 0o755 } else { 0o644 }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn output_mode(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}
