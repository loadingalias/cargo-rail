//! Canonical, backend-independent source tree state.
//!
//! Capture implementations may use Git objects or filesystem reads, but both
//! produce the same sorted tree representation. Host roots and Git object IDs
//! are deliberately excluded because they are lookup context, not source identity.
//! Git worktree capture verifies that Git state, exact source bytes, and file
//! metadata remain stable across the capture window and fails on concurrent drift.
//! Filesystem capture walks one declared root twice, applies only caller-supplied
//! subtree exclusions, and never follows directory symlinks.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::utils;

const GIT_CAPTURE_DRIFT_HELP: &str = "retry after concurrent filesystem and Git operations have finished";
const FILESYSTEM_CAPTURE_DRIFT_HELP: &str = "retry after concurrent filesystem operations have finished";

/// A normalized UTF-8 path relative to the logical source root.
///
/// The source root is the Git worktree for Git-backed capture and the declared
/// Cargo package or workspace root for filesystem-backed capture. Paths use `/`
/// separators, contain no root, prefix, or parent component, and never represent
/// the source root itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepositoryPath(String);

impl RepositoryPath {
  /// Normalize and validate a source-root-relative path.
  ///
  /// # Errors
  ///
  /// Returns an error for empty, absolute, parent-traversing, or non-UTF-8 paths.
  pub fn new(path: &Path) -> RailResult<Self> {
    let mut normalized = String::new();
    for component in path.components() {
      let Component::Normal(component) = component else {
        match component {
          Component::CurDir => continue,
          Component::ParentDir => {
            return Err(RailError::message(format!(
              "source path '{}' must not contain '..'",
              path.display()
            )));
          }
          Component::Prefix(_) | Component::RootDir => {
            return Err(RailError::message(format!(
              "source path '{}' must be relative",
              path.display()
            )));
          }
          Component::Normal(_) => unreachable!(),
        }
      };
      let component = component
        .to_str()
        .ok_or_else(|| RailError::message(format!("source path '{}' is not valid UTF-8", path.display())))?;
      if !normalized.is_empty() {
        normalized.push('/');
      }
      normalized.push_str(component);
    }

    if normalized.is_empty() {
      return Err(RailError::message("source path must identify an entry"));
    }
    Ok(Self(normalized))
  }

  /// Return the canonical `/`-separated representation.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// Return the canonical path as a platform path.
  pub fn as_path(&self) -> &Path {
    Path::new(&self.0)
  }
}

impl fmt::Display for RepositoryPath {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(&self.0)
  }
}

/// SHA-256 identity of exact regular-file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
  /// Digest exact file bytes.
  pub fn sha256(bytes: &[u8]) -> Self {
    crate::instrumentation::record_hash(bytes.len());
    Self(Sha256::digest(bytes).into())
  }

  /// Return the raw SHA-256 bytes.
  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }
}

impl fmt::Display for ContentDigest {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in self.0 {
      write!(formatter, "{byte:02x}")?;
    }
    Ok(())
  }
}

/// Canonical state of one source entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceEntryKind {
  /// Exact regular-file contents and executable mode.
  RegularFile {
    /// SHA-256 of the exact file bytes.
    digest: ContentDigest,
    /// Whether the executable bit is set.
    executable: bool,
  },
  /// A symbolic link whose target is retained exactly as UTF-8 text.
  Symlink {
    /// Link target without path resolution or normalization.
    target: String,
  },
  /// Absence relative to another tree.
  ///
  /// Deletions belong in change state and are rejected from a final [`SourceTree`].
  Deleted,
}

/// One canonical source-root-relative entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTreeEntry {
  /// Canonical source-root-relative path.
  pub path: RepositoryPath,
  /// File, symlink, or deletion state.
  pub kind: SourceEntryKind,
}

impl SourceTreeEntry {
  /// Construct a regular-file entry.
  ///
  /// # Errors
  ///
  /// Returns an error when `path` is not canonicalizable as a source path.
  pub fn regular_file(path: &Path, digest: ContentDigest, executable: bool) -> RailResult<Self> {
    Ok(Self {
      path: RepositoryPath::new(path)?,
      kind: SourceEntryKind::RegularFile { digest, executable },
    })
  }

  /// Construct a symbolic-link entry without resolving its target.
  ///
  /// # Errors
  ///
  /// Returns an error when the entry path or link target is not valid UTF-8.
  pub fn symlink(path: &Path, target: &Path) -> RailResult<Self> {
    let path = RepositoryPath::new(path)?;
    let target = target
      .to_str()
      .ok_or_else(|| RailError::message(format!("symlink target '{}' is not valid UTF-8", target.display())))?;
    Ok(Self {
      path,
      kind: SourceEntryKind::Symlink {
        target: target.to_string(),
      },
    })
  }

  /// Construct a deletion entry for future change-set use.
  ///
  /// # Errors
  ///
  /// Returns an error when `path` is not canonicalizable as a source path.
  pub fn deleted(path: &Path) -> RailResult<Self> {
    Ok(Self {
      path: RepositoryPath::new(path)?,
      kind: SourceEntryKind::Deleted,
    })
  }
}

/// A sorted final source tree with exactly one present entry per path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTree(Vec<SourceTreeEntry>);

impl SourceTree {
  /// Sort and validate final-tree entries.
  ///
  /// # Errors
  ///
  /// Returns an error for duplicate paths or deletion entries.
  pub fn new(mut entries: Vec<SourceTreeEntry>) -> RailResult<Self> {
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));

    for entry in &entries {
      if matches!(entry.kind, SourceEntryKind::Deleted) {
        return Err(RailError::message(format!(
          "final source tree cannot contain deletion '{}'",
          entry.path
        )));
      }
    }
    for pair in entries.windows(2) {
      if pair[0].path == pair[1].path {
        return Err(RailError::message(format!(
          "final source tree contains duplicate path '{}'",
          pair[0].path
        )));
      }
    }

    Ok(Self(entries))
  }

  /// Return entries in canonical path order.
  pub fn entries(&self) -> &[SourceTreeEntry] {
    &self.0
  }
}

/// Final effect of a source change at one canonical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceChangeKind {
  /// The final tree contains a path absent from the base tree.
  Added,
  /// The path exists in both trees with different contents or executable mode.
  Modified,
  /// The path changed between regular-file and symbolic-link state.
  TypeChanged,
  /// The base tree contains a path absent from the final tree.
  Deleted,
}

/// Relationship retained when Git identifies a rename or copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeRelation {
  /// This added path was renamed from the referenced deleted path.
  RenamedFrom(RepositoryPath),
  /// This deleted path was renamed to the referenced added path.
  RenamedTo(RepositoryPath),
  /// This added path copied an otherwise unchanged source path.
  CopiedFrom(RepositoryPath),
}

/// Source layer that contributed to a final path change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeLayer {
  /// A commit between the selected base and `HEAD` changed the path.
  Committed,
  /// The index differs from `HEAD` at the path.
  Staged,
  /// The worktree differs from the index at the path.
  Unstaged,
  /// The path is untracked and not ignored.
  Untracked,
}

/// Compact set of independent source layers contributing to a path change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangeProvenance(u8);

impl ChangeProvenance {
  const COMMITTED: u8 = 1 << 0;
  const STAGED: u8 = 1 << 1;
  const UNSTAGED: u8 = 1 << 2;
  const UNTRACKED: u8 = 1 << 3;

  fn insert(&mut self, layer: ChangeLayer) {
    self.0 |= match layer {
      ChangeLayer::Committed => Self::COMMITTED,
      ChangeLayer::Staged => Self::STAGED,
      ChangeLayer::Unstaged => Self::UNSTAGED,
      ChangeLayer::Untracked => Self::UNTRACKED,
    };
  }

  /// Return whether this change includes the requested layer.
  pub fn contains(self, layer: ChangeLayer) -> bool {
    let mask = match layer {
      ChangeLayer::Committed => Self::COMMITTED,
      ChangeLayer::Staged => Self::STAGED,
      ChangeLayer::Unstaged => Self::UNSTAGED,
      ChangeLayer::Untracked => Self::UNTRACKED,
    };
    self.0 & mask != 0
  }
}

/// One final base-to-worktree path change with layer provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceChange {
  /// Canonical source-root-relative path changed in the final tree.
  pub path: RepositoryPath,
  /// Final effect at this path.
  pub kind: SourceChangeKind,
  /// Rename or copy relationship, when detected.
  pub relation: Option<ChangeRelation>,
  /// Every source layer that contributed to the final path state.
  pub provenance: ChangeProvenance,
}

/// Sorted base-to-final-tree changes with at most one record per path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet(Vec<SourceChange>);

impl ChangeSet {
  /// Return changes in canonical path order.
  pub fn entries(&self) -> &[SourceChange] {
    &self.0
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SourceExclusions(Vec<RepositoryPath>);

impl SourceExclusions {
  pub(crate) fn from_absolute_roots(git: &SystemGit, roots: &[PathBuf]) -> RailResult<Self> {
    let mut exclusions = Self::default();
    exclusions.extend_absolute_roots(git, roots)?;
    Ok(exclusions)
  }

  fn from_filesystem_roots(filesystem_root: &Path, roots: &[PathBuf]) -> RailResult<Self> {
    let filesystem_root = utils::canonicalize_existing(filesystem_root)?;
    let mut exclusions = Self::default();
    for root in roots {
      let root = if root.is_absolute() {
        root.clone()
      } else {
        filesystem_root.join(root)
      };
      match resolve_exclusion_root(&filesystem_root, &root)? {
        ResolvedExclusionRoot::Outside => {}
        ResolvedExclusionRoot::Boundary => {
          return Err(RailError::with_help(
            format!(
              "ignored source root '{}' resolves to the filesystem source root",
              root.display()
            ),
            "ignore only generated subdirectories or select a narrower source root",
          ));
        }
        ResolvedExclusionRoot::Descendant(path) => exclusions.insert(path),
      }
    }
    Ok(exclusions)
  }

  fn extend_absolute_roots(&mut self, git: &SystemGit, roots: &[PathBuf]) -> RailResult<()> {
    let worktree_root = utils::canonicalize_existing(&git.worktree_root)?;
    for root in roots {
      match resolve_exclusion_root(&worktree_root, root)? {
        ResolvedExclusionRoot::Outside => {}
        ResolvedExclusionRoot::Boundary => {
          return Err(RailError::with_help(
            format!(
              "generated-state root '{}' resolves to the Git worktree root",
              root.display()
            ),
            "configure Cargo build output below or outside the repository root",
          ));
        }
        ResolvedExclusionRoot::Descendant(path) => self.insert(path),
      }
    }
    self.0.sort_unstable();
    Ok(())
  }

  fn insert(&mut self, path: RepositoryPath) {
    if self.contains(&path) {
      return;
    }
    self.0.retain(|existing| !path_is_within(existing, &path));
    self.0.push(path);
    self.0.sort_unstable();
  }

  fn contains(&self, path: &RepositoryPath) -> bool {
    self.0.iter().any(|root| path_is_within(path, root))
  }

  pub(crate) fn contains_path(&self, path: &Path) -> RailResult<bool> {
    Ok(self.contains(&RepositoryPath::new(path)?))
  }
}

enum ResolvedExclusionRoot {
  Outside,
  Boundary,
  Descendant(RepositoryPath),
}

fn resolve_exclusion_root(boundary: &Path, root: &Path) -> RailResult<ResolvedExclusionRoot> {
  if let Ok(relative) = root.strip_prefix(boundary) {
    if relative.as_os_str().is_empty() {
      return Ok(ResolvedExclusionRoot::Boundary);
    }
    if let Ok(relative) = RepositoryPath::new(relative) {
      return Ok(ResolvedExclusionRoot::Descendant(relative));
    }
  }

  let resolved_root = utils::canonicalize_allow_missing(root)?;
  let Ok(relative) = resolved_root.strip_prefix(boundary) else {
    return Ok(ResolvedExclusionRoot::Outside);
  };
  if relative.as_os_str().is_empty() {
    return Ok(ResolvedExclusionRoot::Boundary);
  }
  Ok(ResolvedExclusionRoot::Descendant(RepositoryPath::new(relative)?))
}

fn path_is_within(path: &RepositoryPath, root: &RepositoryPath) -> bool {
  path == root || path.as_path().starts_with(root.as_path())
}

/// Canonical source state and the backend that captured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSnapshot {
  /// Source captured from Git object and worktree state.
  GitBacked(SourceTree),
  /// Source captured directly from one declared filesystem root.
  FilesystemBacked(SourceTree),
}

/// One immutable Git worktree capture.
///
/// The canonical tree and layer state are captured together. Workspace-aware
/// callers additionally exclude their resolved generated-state roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeCapture {
  snapshot: SourceSnapshot,
  state: GitCaptureState,
  filesystem: Vec<FilesystemObservation>,
  exclusions: SourceExclusions,
}

impl GitWorktreeCapture {
  /// Capture the final tree and its current Git layer state.
  ///
  /// # Errors
  ///
  /// Returns an error when Git state is malformed or changes during capture, a
  /// path is not canonical UTF-8, an index is unmerged, an entry type is
  /// unsupported, or source bytes or metadata cannot be read consistently.
  pub fn capture(git: &SystemGit) -> RailResult<Self> {
    Self::capture_excluding(git, &[])
  }

  pub(crate) fn capture_excluding(git: &SystemGit, generated_roots: &[PathBuf]) -> RailResult<Self> {
    let exclusions = SourceExclusions::from_absolute_roots(git, generated_roots)?;
    let state = read_git_capture_state(git, &exclusions)?;
    let (tree, filesystem) = capture_final_tree(git, &state.index, &state.status.untracked)?;
    ensure_git_capture_state_unchanged(git, &state, &exclusions)?;
    validate_filesystem_observations(&git.worktree_root, &tree, &filesystem)?;
    Ok(Self {
      snapshot: SourceSnapshot::GitBacked(tree),
      state,
      filesystem,
      exclusions,
    })
  }

  pub(crate) fn exclude_generated_roots(&mut self, git: &SystemGit, generated_roots: &[PathBuf]) -> RailResult<()> {
    self.exclusions.extend_absolute_roots(git, generated_roots)?;
    self.snapshot.retain(|entry| !self.exclusions.contains(&entry.path));
    self.state.index.retain(|entry| !self.exclusions.contains(&entry.path));
    self
      .state
      .status
      .provenance
      .retain(|path, _| !self.exclusions.contains(path));
    self
      .state
      .status
      .untracked
      .retain(|path| !self.exclusions.contains(path));
    self.filesystem.retain(|entry| !self.exclusions.contains(&entry.path));
    Ok(())
  }

  /// Return the immutable canonical source snapshot.
  pub fn snapshot(&self) -> &SourceSnapshot {
    &self.snapshot
  }

  /// Derive sorted changes from `base` without recapturing the final tree.
  ///
  /// # Errors
  ///
  /// Returns an error when `base` cannot be resolved or Git emits malformed or
  /// unsupported diff state.
  pub fn changes_from(&self, git: &SystemGit, base: &str) -> RailResult<ChangeSet> {
    let mut final_changes = read_diff(git, base, None, &self.exclusions)?;
    if !self.state.status.untracked.is_empty() {
      let base_tree = capture_git_tree(git, base, &self.exclusions)?;
      final_changes = merge_untracked_changes(
        final_changes,
        &self.state.status.untracked,
        &base_tree,
        self.snapshot.tree(),
      )?;
    }

    let mut provenance = self.state.status.provenance.clone();
    if provenance.is_empty() {
      mark_provenance(&mut provenance, &final_changes, ChangeLayer::Committed);
    } else {
      let committed = read_diff(git, base, Some("HEAD"), &self.exclusions)?;
      mark_provenance(&mut provenance, &committed, ChangeLayer::Committed);
      for change in &final_changes {
        if !provenance.contains_key(&change.path) {
          provenance
            .entry(change.path.clone())
            .or_default()
            .insert(ChangeLayer::Unstaged);
        }
      }
    }
    Ok(build_change_set(final_changes, provenance))
  }
}

impl SourceSnapshot {
  /// Return the backend-independent canonical tree.
  pub fn tree(&self) -> &SourceTree {
    match self {
      Self::GitBacked(tree) | Self::FilesystemBacked(tree) => tree,
    }
  }

  fn retain(&mut self, mut predicate: impl FnMut(&SourceTreeEntry) -> bool) {
    match self {
      Self::GitBacked(tree) | Self::FilesystemBacked(tree) => tree.0.retain(&mut predicate),
    }
  }

  /// Capture one declared Cargo package or workspace root without Git.
  ///
  /// The walk is bounded to `root`. Each path in `ignored_roots` excludes that
  /// exact subtree; relative paths are resolved from `root`, and paths outside
  /// `root` have no effect. Git ignore files and ambient ignore configuration
  /// are deliberately not interpreted.
  ///
  /// Symbolic links are retained as entries and never traversed. The complete
  /// walk is repeated, including exact file hashing and metadata checks, and no
  /// snapshot is returned if the two captures differ.
  ///
  /// # Errors
  ///
  /// Returns an error when `root` is not a readable directory, an ignored root
  /// would exclude the entire source boundary, a path or link target is not
  /// UTF-8, an entry type is unsupported, or source state changes during capture.
  pub fn capture_filesystem(root: &Path, ignored_roots: &[PathBuf]) -> RailResult<Self> {
    let root = utils::canonicalize_existing(root).map_err(|error| {
      RailError::message(format!(
        "failed to resolve filesystem source root '{}': {}",
        root.display(),
        error
      ))
    })?;
    let metadata = fs::symlink_metadata(&root).map_err(|error| {
      RailError::message(format!(
        "failed to inspect filesystem source root '{}': {}",
        root.display(),
        error
      ))
    })?;
    if !metadata.is_dir() {
      return Err(RailError::with_help(
        format!("filesystem source root '{}' is not a directory", root.display()),
        "select a Cargo package or workspace directory",
      ));
    }

    let exclusions = SourceExclusions::from_filesystem_roots(&root, ignored_roots)?;
    let captured = capture_filesystem_tree(&root, &exclusions)?;
    let repeated = capture_filesystem_tree(&root, &exclusions)?;
    ensure_filesystem_tree_unchanged(&captured, &repeated)?;
    Ok(Self::FilesystemBacked(captured.tree))
  }

  /// Capture the final Git worktree and changes from `base` to that tree.
  ///
  /// The final tree is built from the current index, tracked filesystem state,
  /// and untracked non-ignored files. Change provenance remains separate from
  /// canonical tree identity.
  ///
  /// # Errors
  ///
  /// Returns an error when Git state is malformed or changes during capture, a
  /// source path is not canonical UTF-8, an index is unmerged, an entry type is
  /// unsupported, or source bytes or metadata cannot be read consistently.
  pub fn capture_git_worktree(git: &SystemGit, base: &str) -> RailResult<(Self, ChangeSet)> {
    let capture = GitWorktreeCapture::capture(git)?;
    let changes = capture.changes_from(git, base)?;
    Ok((capture.snapshot, changes))
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexKind {
  RegularFile { executable: bool },
  Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexEntry {
  path: RepositoryPath,
  object_id: String,
  kind: IndexKind,
  skip_worktree: bool,
}

#[derive(Debug, Clone)]
struct ParsedChange {
  path: RepositoryPath,
  kind: SourceChangeKind,
  relation: Option<ChangeRelation>,
}

#[derive(Debug)]
struct GitTreeEntry {
  path: RepositoryPath,
  object_id: String,
  kind: IndexKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorktreeStatus {
  provenance: BTreeMap<RepositoryPath, ChangeProvenance>,
  untracked: Vec<RepositoryPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitCaptureState {
  head: String,
  index: Vec<IndexEntry>,
  status: WorktreeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilesystemEntryType {
  RegularFile,
  Symlink,
  Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilesystemMetadata {
  entry_type: FilesystemEntryType,
  len: u64,
  modified: SystemTime,
  readonly: bool,
  #[cfg(unix)]
  device: u64,
  #[cfg(unix)]
  inode: u64,
  #[cfg(unix)]
  mode: u32,
  #[cfg(unix)]
  changed_seconds: i64,
  #[cfg(unix)]
  changed_nanoseconds: i64,
  #[cfg(windows)]
  file_attributes: u32,
  #[cfg(windows)]
  creation_time: u64,
  #[cfg(windows)]
  last_write_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilesystemObservation {
  path: RepositoryPath,
  index_kind: Option<IndexKind>,
  metadata: FilesystemMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryObservation {
  path: PathBuf,
  metadata: FilesystemMetadata,
}

#[derive(Debug)]
struct CapturedFilesystemTree {
  tree: SourceTree,
  files: Vec<FilesystemObservation>,
  directories: Vec<DirectoryObservation>,
}

#[derive(Debug)]
struct CapturedFilesystemEntry {
  entry: SourceTreeEntry,
  observation: FilesystemObservation,
}

fn read_git_capture_state(git: &SystemGit, exclusions: &SourceExclusions) -> RailResult<GitCaptureState> {
  Ok(GitCaptureState {
    head: git.head_commit()?,
    index: read_index(git, exclusions)?,
    status: read_worktree_status(git, exclusions)?,
  })
}

fn ensure_git_capture_state_unchanged(
  git: &SystemGit,
  expected: &GitCaptureState,
  exclusions: &SourceExclusions,
) -> RailResult<()> {
  // Read status first so a Git filter or filesystem monitor that mutates the
  // index or HEAD is still observed by the subsequent identity reads.
  let status = read_worktree_status(git, exclusions)?;
  let index = read_index(git, exclusions)?;
  let head = git.head_commit()?;
  if head != expected.head {
    return Err(capture_drift_error("Git HEAD changed during source capture"));
  }
  if index != expected.index {
    return Err(capture_drift_error("Git index changed during source capture"));
  }
  if status != expected.status {
    return Err(capture_drift_error("Git worktree status changed during source capture"));
  }
  Ok(())
}

fn capture_drift_error(message: impl Into<String>) -> RailError {
  RailError::with_help(message, GIT_CAPTURE_DRIFT_HELP)
}

fn filesystem_capture_drift_error(message: impl Into<String>) -> RailError {
  RailError::with_help(message, FILESYSTEM_CAPTURE_DRIFT_HELP)
}

fn capture_drift_error_with_help(message: impl Into<String>, help: &'static str) -> RailError {
  RailError::with_help(message, help)
}

fn read_index(git: &SystemGit, exclusions: &SourceExclusions) -> RailResult<Vec<IndexEntry>> {
  let output = git.run_git_at_worktree_root(&["ls-files", "-v", "--stage", "-z"])?;
  parse_index(&output.stdout, exclusions)
}

fn parse_index(output: &[u8], exclusions: &SourceExclusions) -> RailResult<Vec<IndexEntry>> {
  let mut entries = Vec::new();
  for record in output.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
    let flag = *record
      .first()
      .ok_or_else(|| RailError::message("invalid git index record: missing state flag"))?;
    if record.get(1) != Some(&b' ') {
      return Err(RailError::message("invalid git index record: missing flag separator"));
    }
    let tab = record
      .iter()
      .position(|byte| *byte == b'\t')
      .ok_or_else(|| RailError::message("invalid git index record: missing path separator"))?;
    let metadata = std::str::from_utf8(&record[2..tab])
      .map_err(|_| RailError::message("invalid git index record: metadata is not UTF-8"))?;
    let mut fields = metadata.split_whitespace();
    let mode = fields
      .next()
      .ok_or_else(|| RailError::message("invalid git index record: missing mode"))?;
    let object_id = fields
      .next()
      .ok_or_else(|| RailError::message("invalid git index record: missing object ID"))?;
    let stage = fields
      .next()
      .ok_or_else(|| RailError::message("invalid git index record: missing stage"))?;
    if fields.next().is_some() {
      return Err(RailError::message("invalid git index record: unexpected metadata"));
    }

    let raw_path =
      std::str::from_utf8(&record[tab + 1..]).map_err(|_| RailError::message("git index path is not valid UTF-8"))?;
    let path = RepositoryPath::new(Path::new(raw_path))?;
    if exclusions.contains(&path) {
      continue;
    }
    if flag.is_ascii_lowercase() {
      return Err(RailError::with_help(
        format!("cannot capture assume-unchanged index entry '{}'", path),
        format!(
          "run 'git update-index --no-assume-unchanged -- {}' before retrying",
          path
        ),
      ));
    }
    if stage != "0" {
      return Err(RailError::with_help(
        format!("cannot capture unmerged index entry '{}' at stage {}", path, stage),
        "resolve the index conflict and stage the resolution before retrying",
      ));
    }
    let kind = match mode {
      "100644" => IndexKind::RegularFile { executable: false },
      "100755" => IndexKind::RegularFile { executable: true },
      "120000" => IndexKind::Symlink,
      "160000" => {
        return Err(RailError::with_help(
          format!("cannot capture Git submodule entry '{}'", path),
          "replace the gitlink with declared source files or exclude this operation from snapshot capture",
        ));
      }
      other => {
        return Err(RailError::message(format!(
          "cannot capture unsupported Git mode '{}' at '{}'",
          other, path
        )));
      }
    };
    entries.push(IndexEntry {
      path,
      object_id: object_id.to_string(),
      kind,
      skip_worktree: flag.eq_ignore_ascii_case(&b'S'),
    });
  }
  Ok(entries)
}

fn read_worktree_status(git: &SystemGit, exclusions: &SourceExclusions) -> RailResult<WorktreeStatus> {
  let output = git.run_git_at_worktree_root(&["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
  parse_worktree_status(&output.stdout, exclusions)
}

fn parse_worktree_status(output: &[u8], exclusions: &SourceExclusions) -> RailResult<WorktreeStatus> {
  let mut status = WorktreeStatus::default();
  let mut records = output.split(|byte| *byte == 0).filter(|record| !record.is_empty());
  while let Some(record) = records.next() {
    let text = std::str::from_utf8(record).map_err(|_| RailError::message("Git status path is not valid UTF-8"))?;
    match record.first().copied() {
      Some(b'1') => {
        let fields: Vec<&str> = text.splitn(9, ' ').collect();
        if fields.len() != 9 || fields[1].len() != 2 {
          return Err(RailError::message("invalid Git porcelain-v2 ordinary record"));
        }
        mark_status_path(&mut status, fields[8], fields[1], exclusions)?;
      }
      Some(b'2') => {
        let fields: Vec<&str> = text.splitn(10, ' ').collect();
        if fields.len() != 10 || fields[1].len() != 2 {
          return Err(RailError::message("invalid Git porcelain-v2 rename record"));
        }
        let original = records
          .next()
          .ok_or_else(|| RailError::message("invalid Git porcelain-v2 rename record: missing original path"))?;
        let original =
          std::str::from_utf8(original).map_err(|_| RailError::message("Git rename source path is not valid UTF-8"))?;
        mark_status_path(&mut status, fields[9], fields[1], exclusions)?;
        mark_status_path(&mut status, original, fields[1], exclusions)?;
      }
      Some(b'?') if record.get(1) == Some(&b' ') => {
        let path = RepositoryPath::new(Path::new(&text[2..]))?;
        if exclusions.contains(&path) {
          continue;
        }
        status
          .provenance
          .entry(path.clone())
          .or_default()
          .insert(ChangeLayer::Untracked);
        status.untracked.push(path);
      }
      Some(b'u') => {
        let path = text.splitn(11, ' ').nth(10).unwrap_or("unknown");
        if RepositoryPath::new(Path::new(path)).is_ok_and(|path| exclusions.contains(&path)) {
          continue;
        }
        return Err(RailError::with_help(
          format!("cannot capture worktree with unmerged path '{}'", path),
          "resolve the index conflict and stage the resolution before retrying",
        ));
      }
      _ => return Err(RailError::message("invalid or unsupported Git porcelain-v2 record")),
    }
  }
  status.untracked.sort();
  status.untracked.dedup();
  Ok(status)
}

fn mark_status_path(
  status: &mut WorktreeStatus,
  raw_path: &str,
  xy: &str,
  exclusions: &SourceExclusions,
) -> RailResult<()> {
  let path = RepositoryPath::new(Path::new(raw_path))?;
  if exclusions.contains(&path) {
    return Ok(());
  }
  let bytes = xy.as_bytes();
  let provenance = status.provenance.entry(path).or_default();
  if bytes[0] != b'.' {
    provenance.insert(ChangeLayer::Staged);
  }
  if bytes[1] != b'.' {
    provenance.insert(ChangeLayer::Unstaged);
  }
  Ok(())
}

fn capture_filesystem_tree(root: &Path, exclusions: &SourceExclusions) -> RailResult<CapturedFilesystemTree> {
  let mut entries = Vec::new();
  let mut files = Vec::new();
  let mut directories = Vec::new();
  let mut pending_directories = vec![PathBuf::new()];
  let mut validated_directories = BTreeSet::new();

  while let Some(relative_directory) = pending_directories.pop() {
    let absolute_directory = root.join(&relative_directory);
    let display_path = if relative_directory.as_os_str().is_empty() {
      Path::new(".")
    } else {
      relative_directory.as_path()
    };
    let before = fs::symlink_metadata(&absolute_directory).map_err(|error| {
      filesystem_capture_drift_error(format!(
        "source directory '{}' changed during capture: {}",
        display_path.display(),
        error
      ))
    })?;
    let before = FilesystemMetadata::from_metadata(display_path, &before)?;
    if before.entry_type != FilesystemEntryType::Directory {
      return Err(filesystem_capture_drift_error(format!(
        "source directory '{}' changed type during capture",
        display_path.display()
      )));
    }

    let mut children = Vec::new();
    let directory = fs::read_dir(&absolute_directory).map_err(|error| {
      RailError::message(format!(
        "failed to read source directory '{}': {}",
        display_path.display(),
        error
      ))
    })?;
    for child in directory {
      let child = child.map_err(|error| {
        RailError::message(format!(
          "failed to read an entry in source directory '{}': {}",
          display_path.display(),
          error
        ))
      })?;
      children.push(relative_directory.join(child.file_name()));
    }
    children.sort_unstable();

    let after = fs::symlink_metadata(&absolute_directory).map_err(|error| {
      filesystem_capture_drift_error(format!(
        "source directory '{}' changed during capture: {}",
        display_path.display(),
        error
      ))
    })?;
    let after = FilesystemMetadata::from_metadata(display_path, &after)?;
    if before != after {
      return Err(filesystem_capture_drift_error(format!(
        "source directory metadata for '{}' changed during capture",
        display_path.display()
      )));
    }
    directories.push(DirectoryObservation {
      path: relative_directory,
      metadata: before,
    });

    for child in children.into_iter().rev() {
      let path = RepositoryPath::new(&child)?;
      if exclusions.contains(&path) {
        continue;
      }
      let absolute = root.join(&child);
      let metadata = fs::symlink_metadata(&absolute).map_err(|error| {
        filesystem_capture_drift_error(format!("source '{}' changed during capture: {}", path, error))
      })?;
      if metadata.is_dir() {
        pending_directories.push(child);
        continue;
      }
      let captured = capture_filesystem_entry(
        root,
        &path,
        None,
        &mut validated_directories,
        FILESYSTEM_CAPTURE_DRIFT_HELP,
      )?
      .ok_or_else(|| filesystem_capture_drift_error(format!("source '{}' disappeared during capture", path)))?;
      entries.push(captured.entry);
      files.push(captured.observation);
    }
  }

  files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
  directories.sort_unstable_by(|left, right| left.path.cmp(&right.path));
  Ok(CapturedFilesystemTree {
    tree: SourceTree::new(entries)?,
    files,
    directories,
  })
}

fn ensure_filesystem_tree_unchanged(
  expected: &CapturedFilesystemTree,
  current: &CapturedFilesystemTree,
) -> RailResult<()> {
  if expected.tree != current.tree {
    let path = expected
      .tree
      .entries()
      .iter()
      .zip(current.tree.entries())
      .find(|(left, right)| left != right)
      .map(|(left, right)| left.path.clone().min(right.path.clone()))
      .or_else(|| {
        expected
          .tree
          .entries()
          .get(current.tree.entries().len())
          .or_else(|| current.tree.entries().get(expected.tree.entries().len()))
          .map(|entry| entry.path.clone())
      });
    let message = path.map_or_else(
      || "filesystem source tree changed during capture".to_string(),
      |path| format!("source file '{}' changed during capture", path),
    );
    return Err(filesystem_capture_drift_error(message));
  }

  if let Some(observation) = expected
    .files
    .iter()
    .zip(&current.files)
    .find_map(|(left, right)| (left != right).then_some(left))
  {
    return Err(filesystem_capture_drift_error(format!(
      "source metadata for '{}' changed during capture",
      observation.path
    )));
  }
  if expected.files.len() != current.files.len() {
    return Err(filesystem_capture_drift_error(
      "filesystem source files changed during capture",
    ));
  }

  if let Some(observation) = expected
    .directories
    .iter()
    .zip(&current.directories)
    .find_map(|(left, right)| (left != right).then_some(left))
  {
    return Err(filesystem_capture_drift_error(format!(
      "source directory metadata for '{}' changed during capture",
      if observation.path.as_os_str().is_empty() {
        Path::new(".")
      } else {
        observation.path.as_path()
      }
      .display()
    )));
  }
  if expected.directories.len() != current.directories.len() {
    return Err(filesystem_capture_drift_error(
      "filesystem source directories changed during capture",
    ));
  }
  Ok(())
}

fn capture_final_tree(
  git: &SystemGit,
  index: &[IndexEntry],
  untracked: &[RepositoryPath],
) -> RailResult<(SourceTree, Vec<FilesystemObservation>)> {
  let mut entries = Vec::with_capacity(index.len() + untracked.len());
  let mut sparse = Vec::new();
  let mut filesystem = Vec::with_capacity(index.len() + untracked.len());
  let mut validated_directories = BTreeSet::new();

  for indexed in index {
    match capture_filesystem_entry(
      &git.worktree_root,
      &indexed.path,
      Some(indexed.kind),
      &mut validated_directories,
      GIT_CAPTURE_DRIFT_HELP,
    )? {
      Some(captured) => {
        entries.push(captured.entry);
        filesystem.push(captured.observation);
      }
      None if indexed.skip_worktree => sparse.push(indexed),
      None => {}
    }
  }
  for path in untracked {
    let captured = capture_filesystem_entry(
      &git.worktree_root,
      path,
      None,
      &mut validated_directories,
      GIT_CAPTURE_DRIFT_HELP,
    )?
    .ok_or_else(|| RailError::message(format!("untracked source '{}' disappeared during capture", path)))?;
    entries.push(captured.entry);
    filesystem.push(captured.observation);
  }

  if !sparse.is_empty() {
    let object_ids: Vec<&str> = sparse.iter().map(|entry| entry.object_id.as_str()).collect();
    let blobs = git.read_blobs_bulk(&object_ids)?;
    for (indexed, bytes) in sparse.into_iter().zip(blobs) {
      crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
      entries.push(entry_from_bytes(indexed.path.clone(), indexed.kind, &bytes)?);
    }
  }

  Ok((SourceTree::new(entries)?, filesystem))
}

fn capture_git_tree(git: &SystemGit, revision: &str, exclusions: &SourceExclusions) -> RailResult<SourceTree> {
  let output = git.run_git_at_worktree_root(&["ls-tree", "-r", "-z", "--full-tree", "--end-of-options", revision])?;
  let objects = parse_git_tree(&output.stdout, exclusions)?;
  let object_ids: Vec<&str> = objects.iter().map(|entry| entry.object_id.as_str()).collect();
  let blobs = git.read_blobs_bulk(&object_ids)?;
  let mut entries = Vec::with_capacity(objects.len());
  for (object, bytes) in objects.into_iter().zip(blobs) {
    crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
    entries.push(entry_from_bytes(object.path, object.kind, &bytes)?);
  }
  SourceTree::new(entries)
}

fn parse_git_tree(output: &[u8], exclusions: &SourceExclusions) -> RailResult<Vec<GitTreeEntry>> {
  let mut entries = Vec::new();
  for record in output.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
    let tab = record
      .iter()
      .position(|byte| *byte == b'\t')
      .ok_or_else(|| RailError::message("invalid Git tree record: missing path separator"))?;
    let metadata = std::str::from_utf8(&record[..tab])
      .map_err(|_| RailError::message("invalid Git tree record: metadata is not UTF-8"))?;
    let mut fields = metadata.split_whitespace();
    let mode = fields
      .next()
      .ok_or_else(|| RailError::message("invalid Git tree record: missing mode"))?;
    let object_type = fields
      .next()
      .ok_or_else(|| RailError::message("invalid Git tree record: missing object type"))?;
    let object_id = fields
      .next()
      .ok_or_else(|| RailError::message("invalid Git tree record: missing object ID"))?;
    if fields.next().is_some() {
      return Err(RailError::message("invalid Git tree record: unexpected metadata"));
    }
    let raw_path =
      std::str::from_utf8(&record[tab + 1..]).map_err(|_| RailError::message("Git tree path is not valid UTF-8"))?;
    let path = RepositoryPath::new(Path::new(raw_path))?;
    if exclusions.contains(&path) {
      continue;
    }
    let kind = match (mode, object_type) {
      ("100644", "blob") => IndexKind::RegularFile { executable: false },
      ("100755", "blob") => IndexKind::RegularFile { executable: true },
      ("120000", "blob") => IndexKind::Symlink,
      ("160000", "commit") => {
        return Err(RailError::with_help(
          format!("cannot capture Git submodule entry '{}'", path),
          "replace the gitlink with declared source files or exclude this operation from snapshot capture",
        ));
      }
      _ => {
        return Err(RailError::message(format!(
          "cannot capture unsupported Git tree entry '{} {}' at '{}'",
          mode, object_type, path
        )));
      }
    };
    entries.push(GitTreeEntry {
      path,
      object_id: object_id.to_string(),
      kind,
    });
  }
  Ok(entries)
}

fn capture_filesystem_entry(
  root: &Path,
  path: &RepositoryPath,
  index_kind: Option<IndexKind>,
  validated_directories: &mut BTreeSet<PathBuf>,
  drift_help: &'static str,
) -> RailResult<Option<CapturedFilesystemEntry>> {
  reject_symlink_ancestors(root, path, validated_directories)?;
  let absolute = root.join(path.as_path());
  let metadata = match fs::symlink_metadata(&absolute) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to inspect source '{}': {}",
        path, error
      )));
    }
  };
  let before = FilesystemMetadata::from_metadata(path.as_path(), &metadata)?;

  let entry = if metadata.file_type().is_symlink() {
    let target = fs::read_link(&absolute)
      .map_err(|error| RailError::message(format!("failed to read symlink '{}': {}", path, error)))?;
    SourceTreeEntry::symlink(path.as_path(), &target)?
  } else if metadata.is_file() {
    let bytes = fs::read(&absolute)
      .map_err(|error| RailError::message(format!("failed to read source file '{}': {}", path, error)))?;
    crate::instrumentation::record_hashed_file_bytes_read(bytes.len());

    #[cfg(windows)]
    if matches!(index_kind, Some(IndexKind::Symlink)) {
      let target = std::str::from_utf8(&bytes)
        .map_err(|_| RailError::message(format!("symlink target at '{}' is not valid UTF-8", path)))?;
      let entry = SourceTreeEntry::symlink(path.as_path(), Path::new(target))?;
      return finish_filesystem_capture(&absolute, path, index_kind, before, entry, drift_help).map(Some);
    }

    let executable = executable_mode(&metadata, index_kind);
    SourceTreeEntry::regular_file(path.as_path(), ContentDigest::sha256(&bytes), executable)?
  } else {
    return Err(RailError::with_help(
      format!("cannot capture unsupported filesystem object '{}'", path),
      "replace it with a regular file or symbolic link before retrying",
    ));
  };

  finish_filesystem_capture(&absolute, path, index_kind, before, entry, drift_help).map(Some)
}

fn finish_filesystem_capture(
  absolute: &Path,
  path: &RepositoryPath,
  index_kind: Option<IndexKind>,
  before: FilesystemMetadata,
  entry: SourceTreeEntry,
  drift_help: &'static str,
) -> RailResult<CapturedFilesystemEntry> {
  let after = fs::symlink_metadata(absolute).map_err(|error| {
    capture_drift_error_with_help(
      format!("source '{}' changed while it was being read: {}", path, error),
      drift_help,
    )
  })?;
  let after = FilesystemMetadata::from_metadata(path.as_path(), &after)?;
  if before != after {
    return Err(capture_drift_error_with_help(
      format!("source metadata for '{}' changed while it was being read", path),
      drift_help,
    ));
  }
  Ok(CapturedFilesystemEntry {
    entry,
    observation: FilesystemObservation {
      path: path.clone(),
      index_kind,
      metadata: before,
    },
  })
}

impl FilesystemMetadata {
  fn from_metadata(path: &Path, metadata: &fs::Metadata) -> RailResult<Self> {
    let entry_type = if metadata.file_type().is_symlink() {
      FilesystemEntryType::Symlink
    } else if metadata.is_file() {
      FilesystemEntryType::RegularFile
    } else if metadata.is_dir() {
      FilesystemEntryType::Directory
    } else {
      return Err(RailError::with_help(
        format!("cannot capture unsupported filesystem object '{}'", path.display()),
        "replace it with a regular file or symbolic link before retrying",
      ));
    };
    let modified = metadata.modified().map_err(|error| {
      RailError::message(format!(
        "failed to read source metadata for '{}': {}",
        path.display(),
        error
      ))
    })?;

    #[cfg(unix)]
    {
      use std::os::unix::fs::MetadataExt as _;
      Ok(Self {
        entry_type,
        len: metadata.len(),
        modified,
        readonly: metadata.permissions().readonly(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
      })
    }

    #[cfg(windows)]
    {
      use std::os::windows::fs::MetadataExt as _;
      Ok(Self {
        entry_type,
        len: metadata.len(),
        modified,
        readonly: metadata.permissions().readonly(),
        file_attributes: metadata.file_attributes(),
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
      })
    }

    #[cfg(not(any(unix, windows)))]
    {
      Ok(Self {
        entry_type,
        len: metadata.len(),
        modified,
        readonly: metadata.permissions().readonly(),
      })
    }
  }
}

fn validate_filesystem_observations(
  root: &Path,
  tree: &SourceTree,
  observations: &[FilesystemObservation],
) -> RailResult<()> {
  let mut validated_directories = BTreeSet::new();
  for expected in observations {
    let current = capture_filesystem_entry(
      root,
      &expected.path,
      expected.index_kind,
      &mut validated_directories,
      GIT_CAPTURE_DRIFT_HELP,
    )?
    .ok_or_else(|| capture_drift_error(format!("source '{}' disappeared during capture", expected.path)))?;
    let captured = tree_entry(tree, &expected.path).ok_or_else(|| {
      RailError::message(format!(
        "captured source '{}' is absent from the final tree",
        expected.path
      ))
    })?;
    if current.entry.kind != captured.kind {
      return Err(capture_drift_error(format!(
        "source file '{}' changed during capture",
        expected.path
      )));
    }
    if current.observation.metadata != expected.metadata {
      return Err(capture_drift_error(format!(
        "source metadata for '{}' changed during capture",
        expected.path
      )));
    }
  }
  Ok(())
}

fn reject_symlink_ancestors(
  root: &Path,
  path: &RepositoryPath,
  validated_directories: &mut BTreeSet<PathBuf>,
) -> RailResult<()> {
  let mut absolute = root.to_path_buf();
  let mut relative = PathBuf::new();
  let Some(parent) = path.as_path().parent() else {
    return Ok(());
  };
  for component in parent.components() {
    absolute.push(component);
    relative.push(component);
    if validated_directories.contains(&relative) {
      continue;
    }
    let metadata = match fs::symlink_metadata(&absolute) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
      Err(error) => {
        return Err(RailError::message(format!(
          "failed to inspect source ancestor '{}' for '{}': {}",
          absolute.display(),
          path,
          error
        )));
      }
    };
    if metadata.file_type().is_symlink() {
      return Err(RailError::with_help(
        format!("cannot capture source '{}' through symbolic-link ancestor", path),
        "restore the tracked directory structure before retrying",
      ));
    }
    if !metadata.is_dir() {
      return Err(RailError::with_help(
        format!("cannot capture source '{}' through non-directory ancestor", path),
        "restore the tracked directory structure before retrying",
      ));
    }
    validated_directories.insert(relative.clone());
  }
  Ok(())
}

#[cfg(unix)]
fn executable_mode(metadata: &fs::Metadata, _index_kind: Option<IndexKind>) -> bool {
  use std::os::unix::fs::PermissionsExt as _;
  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_mode(_metadata: &fs::Metadata, index_kind: Option<IndexKind>) -> bool {
  matches!(index_kind, Some(IndexKind::RegularFile { executable: true }))
}

fn entry_from_bytes(path: RepositoryPath, kind: IndexKind, bytes: &[u8]) -> RailResult<SourceTreeEntry> {
  match kind {
    IndexKind::RegularFile { executable } => {
      SourceTreeEntry::regular_file(path.as_path(), ContentDigest::sha256(bytes), executable)
    }
    IndexKind::Symlink => {
      let target = std::str::from_utf8(bytes)
        .map_err(|_| RailError::message(format!("symlink target at '{}' is not valid UTF-8", path)))?;
      SourceTreeEntry::symlink(path.as_path(), Path::new(target))
    }
  }
}

fn read_diff(
  git: &SystemGit,
  base: &str,
  head: Option<&str>,
  exclusions: &SourceExclusions,
) -> RailResult<Vec<ParsedChange>> {
  let mut args = vec![
    "diff",
    "--name-status",
    "-z",
    "--find-renames",
    "--find-copies-harder",
    "--no-ext-diff",
    "--end-of-options",
    base,
  ];
  if let Some(head) = head {
    args.push(head);
  }
  args.push("--");
  let output = git.run_git_at_worktree_root(&args)?;
  let mut changes = parse_name_status(&output.stdout)?;
  changes.retain(|change| !exclusions.contains(&change.path));
  for change in &mut changes {
    let related_path = match &change.relation {
      Some(ChangeRelation::RenamedFrom(path) | ChangeRelation::RenamedTo(path) | ChangeRelation::CopiedFrom(path)) => {
        Some(path)
      }
      None => None,
    };
    if related_path.is_some_and(|path| exclusions.contains(path)) {
      change.relation = None;
    }
  }
  Ok(changes)
}

fn parse_name_status(output: &[u8]) -> RailResult<Vec<ParsedChange>> {
  let mut changes = Vec::new();
  let mut fields = output.split(|byte| *byte == 0).filter(|field| !field.is_empty());
  while let Some(status) = fields.next() {
    let status = std::str::from_utf8(status).map_err(|_| RailError::message("Git diff status is not valid UTF-8"))?;
    let code = status
      .as_bytes()
      .first()
      .copied()
      .ok_or_else(|| RailError::message("Git diff emitted an empty status"))?;
    match code {
      b'A' | b'M' | b'D' | b'T' => {
        let path = next_diff_path(&mut fields, status)?;
        let kind = match code {
          b'A' => SourceChangeKind::Added,
          b'M' => SourceChangeKind::Modified,
          b'D' => SourceChangeKind::Deleted,
          b'T' => SourceChangeKind::TypeChanged,
          _ => unreachable!(),
        };
        changes.push(ParsedChange {
          path,
          kind,
          relation: None,
        });
      }
      b'R' => {
        let source = next_diff_path(&mut fields, status)?;
        let destination = next_diff_path(&mut fields, status)?;
        changes.push(ParsedChange {
          path: source.clone(),
          kind: SourceChangeKind::Deleted,
          relation: Some(ChangeRelation::RenamedTo(destination.clone())),
        });
        changes.push(ParsedChange {
          path: destination,
          kind: SourceChangeKind::Added,
          relation: Some(ChangeRelation::RenamedFrom(source)),
        });
      }
      b'C' => {
        let source = next_diff_path(&mut fields, status)?;
        let destination = next_diff_path(&mut fields, status)?;
        changes.push(ParsedChange {
          path: destination,
          kind: SourceChangeKind::Added,
          relation: Some(ChangeRelation::CopiedFrom(source)),
        });
      }
      other => {
        return Err(RailError::message(format!(
          "unsupported Git diff status '{}'",
          char::from(other)
        )));
      }
    }
  }
  Ok(changes)
}

fn next_diff_path<'a>(fields: &mut impl Iterator<Item = &'a [u8]>, status: &str) -> RailResult<RepositoryPath> {
  let raw = fields
    .next()
    .ok_or_else(|| RailError::message(format!("Git diff status '{}' is missing a path", status)))?;
  let raw = std::str::from_utf8(raw).map_err(|_| RailError::message("Git diff path is not valid UTF-8"))?;
  RepositoryPath::new(Path::new(raw))
}

fn merge_untracked_changes(
  tracked_changes: Vec<ParsedChange>,
  untracked: &[RepositoryPath],
  base: &SourceTree,
  final_tree: &SourceTree,
) -> RailResult<Vec<ParsedChange>> {
  let hints = tracked_changes.clone();
  let mut changes: BTreeMap<RepositoryPath, ParsedChange> = tracked_changes
    .into_iter()
    .map(|change| (change.path.clone(), change))
    .collect();

  for path in untracked {
    let final_entry = tree_entry(final_tree, path)
      .ok_or_else(|| RailError::message(format!("untracked source '{}' is absent from the final tree", path)))?;
    match tree_entry(base, path) {
      None => {
        changes.insert(
          path.clone(),
          ParsedChange {
            path: path.clone(),
            kind: SourceChangeKind::Added,
            relation: None,
          },
        );
      }
      Some(base_entry) if base_entry.kind == final_entry.kind => {
        changes.remove(path);
      }
      Some(base_entry) => {
        changes.insert(
          path.clone(),
          ParsedChange {
            path: path.clone(),
            kind: if same_entry_type(&base_entry.kind, &final_entry.kind) {
              SourceChangeKind::Modified
            } else {
              SourceChangeKind::TypeChanged
            },
            relation: None,
          },
        );
      }
    }
  }

  let mut changes: Vec<_> = changes.into_values().collect();
  for change in &mut changes {
    change.relation = None;
  }
  apply_change_relations(&mut changes, base, final_tree, &hints);
  Ok(changes)
}

fn same_entry_type(left: &SourceEntryKind, right: &SourceEntryKind) -> bool {
  matches!(
    (left, right),
    (SourceEntryKind::RegularFile { .. }, SourceEntryKind::RegularFile { .. })
      | (SourceEntryKind::Symlink { .. }, SourceEntryKind::Symlink { .. })
      | (SourceEntryKind::Deleted, SourceEntryKind::Deleted)
  )
}

fn apply_change_relations(
  changes: &mut [ParsedChange],
  base: &SourceTree,
  final_tree: &SourceTree,
  hints: &[ParsedChange],
) {
  let indexes: BTreeMap<RepositoryPath, usize> = changes
    .iter()
    .enumerate()
    .map(|(index, change)| (change.path.clone(), index))
    .collect();
  let mut renamed_sources = BTreeSet::new();

  for hint in hints {
    let Some(destination) = indexes.get(&hint.path).copied() else {
      continue;
    };
    if changes[destination].kind != SourceChangeKind::Added {
      continue;
    }
    match &hint.relation {
      Some(ChangeRelation::RenamedFrom(source)) => {
        if let Some(source_index) = indexes.get(source).copied()
          && changes[source_index].kind == SourceChangeKind::Deleted
          && renamed_sources.insert(source.clone())
        {
          changes[source_index].relation = Some(ChangeRelation::RenamedTo(hint.path.clone()));
          changes[destination].relation = Some(ChangeRelation::RenamedFrom(source.clone()));
        } else if tree_path_is_unchanged(base, final_tree, source) {
          changes[destination].relation = Some(ChangeRelation::CopiedFrom(source.clone()));
        }
      }
      Some(ChangeRelation::CopiedFrom(source)) if tree_path_is_unchanged(base, final_tree, source) => {
        changes[destination].relation = Some(ChangeRelation::CopiedFrom(source.clone()));
      }
      _ => {}
    }
  }

  let mut sources_by_content: BTreeMap<SourceEntryKind, Vec<RepositoryPath>> = BTreeMap::new();
  for entry in base.entries() {
    sources_by_content
      .entry(entry.kind.clone())
      .or_default()
      .push(entry.path.clone());
  }

  for destination in 0..changes.len() {
    if changes[destination].kind != SourceChangeKind::Added || changes[destination].relation.is_some() {
      continue;
    }
    let Some(final_entry) = tree_entry(final_tree, &changes[destination].path) else {
      continue;
    };
    let Some(candidates) = sources_by_content.get(&final_entry.kind) else {
      continue;
    };

    let rename_source = candidates.iter().find_map(|source| {
      let source_index = indexes.get(source).copied()?;
      (changes[source_index].kind == SourceChangeKind::Deleted && !renamed_sources.contains(source))
        .then_some((source.clone(), source_index))
    });
    if let Some((source, source_index)) = rename_source {
      renamed_sources.insert(source.clone());
      changes[source_index].relation = Some(ChangeRelation::RenamedTo(changes[destination].path.clone()));
      changes[destination].relation = Some(ChangeRelation::RenamedFrom(source));
      continue;
    }

    if let Some(source) = candidates
      .iter()
      .find(|source| tree_path_is_unchanged(base, final_tree, source))
    {
      changes[destination].relation = Some(ChangeRelation::CopiedFrom(source.clone()));
    }
  }
}

fn tree_entry<'a>(tree: &'a SourceTree, path: &RepositoryPath) -> Option<&'a SourceTreeEntry> {
  tree
    .entries()
    .binary_search_by(|entry| entry.path.cmp(path))
    .ok()
    .map(|index| &tree.entries()[index])
}

fn tree_path_is_unchanged(base: &SourceTree, final_tree: &SourceTree, path: &RepositoryPath) -> bool {
  match (tree_entry(base, path), tree_entry(final_tree, path)) {
    (Some(base_entry), Some(final_entry)) => base_entry.kind == final_entry.kind,
    _ => false,
  }
}

fn mark_provenance(
  provenance: &mut BTreeMap<RepositoryPath, ChangeProvenance>,
  changes: &[ParsedChange],
  layer: ChangeLayer,
) {
  for change in changes {
    provenance.entry(change.path.clone()).or_default().insert(layer);
  }
}

fn build_change_set(
  final_changes: Vec<ParsedChange>,
  mut provenance: BTreeMap<RepositoryPath, ChangeProvenance>,
) -> ChangeSet {
  ChangeSet(
    final_changes
      .into_iter()
      .map(|parsed| SourceChange {
        provenance: provenance.remove(&parsed.path).unwrap_or_default(),
        path: parsed.path,
        kind: parsed.kind,
        relation: parsed.relation,
      })
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn repository_paths_are_normalized_and_bounded() {
    let path = RepositoryPath::new(Path::new("./crates/demo/./src/lib.rs")).unwrap();
    assert_eq!(path.as_str(), "crates/demo/src/lib.rs");

    for invalid in ["", ".", "../escape", "crate/../../escape", "/absolute"] {
      assert!(RepositoryPath::new(Path::new(invalid)).is_err(), "accepted {invalid:?}");
    }
  }

  #[cfg(unix)]
  #[test]
  fn repository_paths_and_symlink_targets_reject_non_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let invalid = Path::new(OsStr::from_bytes(b"invalid-\xff"));
    assert!(RepositoryPath::new(invalid).is_err());
    assert!(SourceTreeEntry::symlink(Path::new("link"), invalid).is_err());
  }

  #[test]
  fn entries_preserve_content_mode_target_and_deletion() {
    let digest = ContentDigest::sha256(b"abc");
    assert_eq!(
      digest.to_string(),
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );

    let file = SourceTreeEntry::regular_file(Path::new("bin/tool"), digest, true).unwrap();
    assert_eq!(
      file.kind,
      SourceEntryKind::RegularFile {
        digest,
        executable: true
      }
    );

    let link = SourceTreeEntry::symlink(Path::new("current"), Path::new("../releases/./latest")).unwrap();
    assert_eq!(
      link.kind,
      SourceEntryKind::Symlink {
        target: "../releases/./latest".to_string()
      }
    );

    let deleted = SourceTreeEntry::deleted(Path::new("removed.rs")).unwrap();
    assert_eq!(deleted.kind, SourceEntryKind::Deleted);
  }

  #[test]
  fn final_trees_sort_entries_and_reject_invalid_states() {
    let digest = ContentDigest::sha256(b"content");
    let tree = SourceTree::new(vec![
      SourceTreeEntry::regular_file(Path::new("z.rs"), digest, false).unwrap(),
      SourceTreeEntry::symlink(Path::new("a.rs"), Path::new("z.rs")).unwrap(),
    ])
    .unwrap();
    assert_eq!(
      tree
        .entries()
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>(),
      ["a.rs", "z.rs"]
    );

    let duplicate = SourceTree::new(vec![
      SourceTreeEntry::regular_file(Path::new("src/./lib.rs"), digest, false).unwrap(),
      SourceTreeEntry::regular_file(Path::new("src/lib.rs"), digest, true).unwrap(),
    ])
    .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate path 'src/lib.rs'"));

    let deletion = SourceTree::new(vec![SourceTreeEntry::deleted(Path::new("gone.rs")).unwrap()]).unwrap_err();
    assert!(deletion.to_string().contains("cannot contain deletion 'gone.rs'"));
  }

  #[test]
  fn both_backends_expose_the_same_tree_contract() {
    let tree = SourceTree::new(vec![]).unwrap();
    let git = SourceSnapshot::GitBacked(tree.clone());
    let filesystem = SourceSnapshot::FilesystemBacked(tree);
    assert_eq!(git.tree().entries(), filesystem.tree().entries());
  }

  #[test]
  fn capture_rejects_head_index_and_status_drift_independently() {
    let temp = tempfile::TempDir::new().unwrap();
    crate::git::init_repo(temp.path(), "main").unwrap();
    let git = SystemGit::open(temp.path()).unwrap();
    git.set_config("user.name", "Test User").unwrap();
    git.set_config("user.email", "test@example.com").unwrap();
    fs::write(temp.path().join("tracked.txt"), "before\n").unwrap();
    git.stage_all().unwrap();
    git.commit("initial").unwrap();
    let exclusions = SourceExclusions::default();

    let clean = read_git_capture_state(&git, &exclusions).unwrap();
    fs::write(temp.path().join("untracked.txt"), "untracked\n").unwrap();
    let error = ensure_git_capture_state_unchanged(&git, &clean, &exclusions).unwrap_err();
    assert!(
      error
        .to_string()
        .contains("Git worktree status changed during source capture")
    );
    fs::remove_file(temp.path().join("untracked.txt")).unwrap();

    let clean = read_git_capture_state(&git, &exclusions).unwrap();
    fs::write(temp.path().join("tracked.txt"), "staged\n").unwrap();
    git.stage_all().unwrap();
    let error = ensure_git_capture_state_unchanged(&git, &clean, &exclusions).unwrap_err();
    assert!(error.to_string().contains("Git index changed during source capture"));

    let staged = read_git_capture_state(&git, &exclusions).unwrap();
    git.commit("change HEAD").unwrap();
    let error = ensure_git_capture_state_unchanged(&git, &staged, &exclusions).unwrap_err();
    assert!(error.to_string().contains("Git HEAD changed during source capture"));
  }

  #[test]
  fn capture_rejects_exact_content_and_metadata_drift() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = RepositoryPath::new(Path::new("source.txt")).unwrap();
    let absolute = temp.path().join(path.as_path());
    fs::write(&absolute, "before!!\n").unwrap();

    let captured = capture_filesystem_entry(temp.path(), &path, None, &mut BTreeSet::new(), GIT_CAPTURE_DRIFT_HELP)
      .unwrap()
      .unwrap();
    let tree = SourceTree::new(vec![captured.entry]).unwrap();
    let observations = vec![captured.observation];
    fs::write(&absolute, "after!!!\n").unwrap();
    let error = validate_filesystem_observations(temp.path(), &tree, &observations).unwrap_err();
    assert!(
      error
        .to_string()
        .contains("source file 'source.txt' changed during capture")
    );

    fs::write(&absolute, "stable\n").unwrap();
    let captured = capture_filesystem_entry(temp.path(), &path, None, &mut BTreeSet::new(), GIT_CAPTURE_DRIFT_HELP)
      .unwrap()
      .unwrap();
    let tree = SourceTree::new(vec![captured.entry]).unwrap();
    let observations = vec![captured.observation];
    let original_permissions = fs::metadata(&absolute).unwrap().permissions();
    let mut changed_permissions = original_permissions.clone();
    changed_permissions.set_readonly(true);
    fs::set_permissions(&absolute, changed_permissions).unwrap();
    let error = validate_filesystem_observations(temp.path(), &tree, &observations).unwrap_err();
    fs::set_permissions(&absolute, original_permissions).unwrap();
    assert!(
      error
        .to_string()
        .contains("source metadata for 'source.txt' changed during capture")
    );
  }

  #[test]
  fn filesystem_capture_comparison_rejects_exact_tree_and_directory_drift() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("source.txt"), "before!!\n").unwrap();
    let exclusions = SourceExclusions::default();

    let captured = capture_filesystem_tree(temp.path(), &exclusions).unwrap();
    fs::write(temp.path().join("source.txt"), "after!!!\n").unwrap();
    let changed = capture_filesystem_tree(temp.path(), &exclusions).unwrap();
    let error = ensure_filesystem_tree_unchanged(&captured, &changed).unwrap_err();
    assert!(
      error
        .to_string()
        .contains("source file 'source.txt' changed during capture")
    );
    assert_eq!(error.help_message().as_deref(), Some(FILESYSTEM_CAPTURE_DRIFT_HELP));

    let captured = capture_filesystem_tree(temp.path(), &exclusions).unwrap();
    fs::create_dir(temp.path().join("new-empty-directory")).unwrap();
    let changed = capture_filesystem_tree(temp.path(), &exclusions).unwrap();
    let error = ensure_filesystem_tree_unchanged(&captured, &changed).unwrap_err();
    assert!(error.to_string().contains("source directory"));
    assert!(error.to_string().contains("changed during capture"));
  }

  #[test]
  fn filesystem_capture_rejects_ignoring_the_declared_root() {
    let temp = tempfile::TempDir::new().unwrap();
    let error = SourceSnapshot::capture_filesystem(temp.path(), &[PathBuf::from(".")]).unwrap_err();
    assert!(error.to_string().contains("resolves to the filesystem source root"));
    assert_eq!(
      error.help_message().as_deref(),
      Some("ignore only generated subdirectories or select a narrower source root")
    );
  }

  #[test]
  fn index_parser_retains_modes_and_rejects_unmerged_entries() {
    let parsed = parse_index(
      b"H 100644 1111111111111111111111111111111111111111 0\tsrc/lib.rs\0\
        S 100755 2222222222222222222222222222222222222222 0\tbin/tool\0\
        H 120000 3333333333333333333333333333333333333333 0\tcurrent\0",
      &SourceExclusions::default(),
    )
    .unwrap();
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].path.as_str(), "src/lib.rs");
    assert_eq!(parsed[0].kind, IndexKind::RegularFile { executable: false });
    assert_eq!(parsed[1].kind, IndexKind::RegularFile { executable: true });
    assert!(parsed[1].skip_worktree);
    assert_eq!(parsed[2].kind, IndexKind::Symlink);

    let error = parse_index(
      b"H 100644 1111111111111111111111111111111111111111 2\tsrc/lib.rs\0",
      &SourceExclusions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("unmerged index entry 'src/lib.rs'"));

    let error = parse_index(
      b"h 100644 1111111111111111111111111111111111111111 0\tsrc/lib.rs\0",
      &SourceExclusions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("assume-unchanged index entry 'src/lib.rs'"));
  }

  #[test]
  fn porcelain_parser_keeps_independent_layers_and_both_rename_paths() {
    let parsed = parse_worktree_status(
      b"1 M. N... 100644 100644 100644 a b staged.rs\0\
        1 .M N... 100644 100644 100644 a b unstaged.rs\0\
        1 MM N... 100644 100644 100644 a b both.rs\0\
        2 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0\
        ? untracked.rs\0",
      &SourceExclusions::default(),
    )
    .unwrap();

    let provenance = |path: &str| parsed.provenance[&RepositoryPath::new(Path::new(path)).unwrap()];
    assert!(provenance("staged.rs").contains(ChangeLayer::Staged));
    assert!(!provenance("staged.rs").contains(ChangeLayer::Unstaged));
    assert!(provenance("unstaged.rs").contains(ChangeLayer::Unstaged));
    assert!(provenance("both.rs").contains(ChangeLayer::Staged));
    assert!(provenance("both.rs").contains(ChangeLayer::Unstaged));
    assert!(provenance("old.rs").contains(ChangeLayer::Staged));
    assert!(provenance("new.rs").contains(ChangeLayer::Staged));
    assert!(provenance("untracked.rs").contains(ChangeLayer::Untracked));
    assert_eq!(parsed.untracked[0].as_str(), "untracked.rs");
  }

  #[test]
  fn name_status_parser_retains_rename_copy_and_type_semantics() {
    let parsed =
      parse_name_status(b"M\0modified.rs\0T\0typed.rs\0R100\0old.rs\0new.rs\0C100\0source.rs\0copy.rs\0D\0gone.rs\0")
        .unwrap();
    assert_eq!(parsed.len(), 6);
    assert_eq!(parsed[0].kind, SourceChangeKind::Modified);
    assert_eq!(parsed[1].kind, SourceChangeKind::TypeChanged);
    assert_eq!(
      parsed[2].relation,
      Some(ChangeRelation::RenamedTo(
        RepositoryPath::new(Path::new("new.rs")).unwrap()
      ))
    );
    assert_eq!(
      parsed[3].relation,
      Some(ChangeRelation::RenamedFrom(
        RepositoryPath::new(Path::new("old.rs")).unwrap()
      ))
    );
    assert_eq!(
      parsed[4].relation,
      Some(ChangeRelation::CopiedFrom(
        RepositoryPath::new(Path::new("source.rs")).unwrap()
      ))
    );
    assert_eq!(parsed[5].kind, SourceChangeKind::Deleted);
  }
}
