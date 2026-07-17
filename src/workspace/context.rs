//! Unified workspace context - build once, pass everywhere
//!
//! # Design
//!
//! WorkspaceContext eliminates redundant metadata/config/graph loads by building
//! all workspace-level data structures once in main.rs, then passing by reference
//! to all commands.
//!
//! # Performance Impact
//!
//! Before: Each command loaded metadata/config independently (50-200ms × N commands)
//! After: Single load in main, shared across all operations (50-200ms total)
//!
//! # Architecture
//!
//! ```text
//! main.rs:
//!   WorkspaceContext::build() -> &WorkspaceContext
//!   |
//!   v
//! commands/split.rs, sync.rs, etc:
//!   fn execute(ctx: &WorkspaceContext)
//! ```

use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::compiler::cfg_eval::{TargetCfgSet, load_target_cfg_sets};
use crate::config::{ConfigLoadResult, RailConfig};
use crate::error::{ConfigError, GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::graph::WorkspaceGraph;
use crate::progress;
use crate::source::GitWorktreeCapture;
use cargo_metadata::{Metadata, MetadataCommand, Package};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Cargo State (merged from cargo_state.rs)

/// Cargo state for the workspace
///
/// Provides cargo metadata and workspace information.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct CargoState {
  /// Underlying cargo metadata
  metadata: Arc<Metadata>,

  /// Cached workspace root
  workspace_root: PathBuf,

  /// Index: package name → index in metadata.packages (workspace members only)
  /// Enables O(1) package lookup instead of O(n) linear scan
  package_index: std::collections::HashMap<String, usize>,

  /// Cached set of proc-macro crate names for O(1) detection
  /// Built once during construction, used by change analysis
  proc_macro_crates: std::collections::HashSet<String>,
}

/// Cache version - increment when MetadataCache format changes
/// This ensures old cache files are automatically invalidated
const CACHE_VERSION: u32 = 2;

/// Metadata cache structure
#[derive(Serialize, Deserialize)]
struct MetadataCache {
  /// Cache format version - used to invalidate on schema changes
  #[serde(default)]
  version: u32,
  /// FNV-1a hash of Cargo.toml + Cargo.lock + all workspace member Cargo.toml manifests
  hash: u64,
  /// Cached metadata
  metadata: Metadata,
}

impl CargoState {
  /// Load cargo metadata from workspace root
  ///
  /// Uses a content-based cache (FNV-1a hash of workspace Cargo.toml/Cargo.lock + all workspace member Cargo.toml)
  /// stored in `target/cargo-rail/metadata.json` to speed up subsequent loads.
  fn load(workspace_root: &Path) -> RailResult<Self> {
    let cache_dir = workspace_root.join("target").join("cargo-rail");
    let cache_file = cache_dir.join("metadata.json");

    // Try to load from cache
    // Cache is valid only if:
    // 1. Version matches (cache format unchanged)
    // 2. Hash matches (workspace + member manifests unchanged)
    // 3. Workspace root path matches (repo hasn't been moved/copied)
    if let Some(cache) = fs::read_to_string(&cache_file)
      .ok()
      .and_then(|s| serde_json::from_str::<MetadataCache>(&s).ok())
      && cache.version == CACHE_VERSION
    {
      // Validate workspace root path matches current location
      // This handles the case where a repo is copied/moved to a different path
      let cached_root = cache.metadata.workspace_root.as_std_path();
      let current_root_canonical = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
      let cached_root_canonical = cached_root.canonicalize().unwrap_or_else(|_| cached_root.to_path_buf());

      if current_root_canonical == cached_root_canonical {
        let current_hash = compute_workspace_hash_with_members(workspace_root, &cache.metadata);
        if cache.hash == current_hash {
          // Cache hit - use cached metadata
          crate::instrumentation::record_cargo_metadata_cache_hit();
          return Ok(Self::from_metadata(Arc::new(cache.metadata)));
        }
      }
      // Hash mismatch or path mismatch - cache is stale, will reload below
    }

    // Cache miss or mismatch - load fresh metadata
    crate::instrumentation::record_cargo_metadata_load(false);
    let metadata = MetadataCommand::new()
      .manifest_path(workspace_root.join("Cargo.toml"))
      .exec()?;

    let metadata = Arc::new(metadata);

    // Save to cache
    if let Ok(()) = fs::create_dir_all(&cache_dir) {
      let current_hash = compute_workspace_hash_with_members(workspace_root, &metadata);
      #[derive(Serialize)]
      struct MetadataCacheRef<'a> {
        version: u32,
        hash: u64,
        metadata: &'a Metadata,
      }
      let cache = MetadataCacheRef {
        version: CACHE_VERSION,
        hash: current_hash,
        metadata: &metadata,
      };
      match serde_json::to_string(&cache) {
        Ok(json) => {
          if let Err(e) = fs::write(&cache_file, json) {
            crate::warn!("failed to write metadata cache {}: {}", cache_file.display(), e);
          }
        }
        Err(e) => {
          crate::warn!(
            "failed to serialize metadata cache {}: {} (proceeding without cache)",
            cache_file.display(),
            e
          );
        }
      }
    }

    Ok(Self::from_metadata(metadata))
  }

  /// Build CargoState from metadata, constructing the package index and proc-macro cache
  fn from_metadata(metadata: Arc<Metadata>) -> Self {
    // Normalize path separators on Windows (cargo metadata uses forward slashes).
    // Using .components().collect() converts to platform-native separators without
    // adding the \\?\ prefix that canonicalize() would add.
    let workspace_root: PathBuf = metadata.workspace_root.as_std_path().components().collect();

    // Build O(1) lookup index: package name → index in metadata.packages
    let workspace_member_ids: std::collections::HashSet<_> = metadata.workspace_members.iter().collect();
    let workspace_packages: Vec<_> = metadata
      .packages
      .iter()
      .enumerate()
      .filter(|(_, pkg)| workspace_member_ids.contains(&pkg.id))
      .collect();

    let package_index = workspace_packages
      .iter()
      .map(|(idx, pkg)| (pkg.name.to_string(), *idx))
      .collect();

    // Build proc-macro crate set for O(1) detection
    let proc_macro_crates = workspace_packages
      .iter()
      .filter(|(_, pkg)| {
        pkg.targets.iter().any(|t| {
          t.kind
            .iter()
            .any(|k| matches!(k, cargo_metadata::TargetKind::ProcMacro))
        })
      })
      .map(|(_, pkg)| pkg.name.to_string())
      .collect();

    Self {
      metadata,
      workspace_root,
      package_index,
      proc_macro_crates,
    }
  }

  /// Get workspace root path
  pub fn workspace_root(&self) -> &Path {
    &self.workspace_root
  }

  /// Get all workspace members
  pub fn workspace_members(&self) -> Vec<&Package> {
    self.metadata.workspace_packages()
  }

  /// Get a specific workspace package by name (O(1) lookup)
  pub fn get_package(&self, name: &str) -> Option<&Package> {
    self.package_index.get(name).map(|&idx| &self.metadata.packages[idx])
  }

  /// Get the underlying metadata (for compatibility)
  pub fn metadata(&self) -> &Metadata {
    &self.metadata
  }

  /// Clone the shared metadata handle for another workspace-level index.
  pub(crate) fn shared_metadata(&self) -> Arc<Metadata> {
    Arc::clone(&self.metadata)
  }

  /// Check if a crate is a proc-macro crate (O(1) lookup)
  pub fn is_proc_macro(&self, crate_name: &str) -> bool {
    self.proc_macro_crates.contains(crate_name)
  }

  /// Get the set of all proc-macro crate names
  pub fn proc_macro_crates(&self) -> &std::collections::HashSet<String> {
    &self.proc_macro_crates
  }

  /// Check if a package is publishable based on Cargo.toml
  ///
  /// Returns `true` if the package can be published to a registry.
  /// Returns `false` if `publish = false` in Cargo.toml.
  ///
  /// Note: This only checks Cargo.toml. For combined Cargo.toml + rail.toml
  /// checking, use `ReleaseValidator::is_publishable()`.
  pub fn is_package_publishable(package: &Package) -> bool {
    // package.publish is Option<Vec<String>>:
    // - None = publishable (no restriction)
    // - Some([]) = publish = false (empty registry list)
    // - Some(["crates-io"]) = can publish to listed registries
    package.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true)
  }

  /// Check if a workspace member is binary-only (has `[[bin]]` targets but no library target).
  ///
  /// This is used by planner/executor flows to optionally skip crates
  /// that can't be selected by `cargo test -p <crate>` (no library target).
  pub fn is_binary_only(&self, crate_name: &str) -> bool {
    let Some(pkg) = self.get_package(crate_name) else {
      return false;
    };

    let mut has_bin = false;
    let mut has_lib_like = false;

    for target in &pkg.targets {
      for kind in &target.kind {
        match kind {
          cargo_metadata::TargetKind::Bin => has_bin = true,
          cargo_metadata::TargetKind::Lib | cargo_metadata::TargetKind::ProcMacro => has_lib_like = true,
          _ => {}
        }
      }
    }

    has_bin && !has_lib_like
  }
}

/// Compute a content-based hash of workspace manifests
///
/// Uses FNV-1a hash of workspace Cargo.toml/Cargo.lock plus all workspace member Cargo.toml
/// manifests to detect changes that invalidate cached cargo metadata.
fn compute_workspace_hash_with_members(workspace_root: &Path, metadata: &Metadata) -> u64 {
  const FNV_PRIME: u64 = 0x100000001b3;
  const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

  let mut hash = FNV_OFFSET_BASIS;
  crate::instrumentation::record_hash_operation();

  fn hash_file(hash: &mut u64, path: &Path, fnv_prime: u64) {
    let Ok(mut file) = fs::File::open(path) else {
      // If the file doesn't exist or can't be opened, still mix in a marker so the hash
      // is very unlikely to match a previous state where it was readable.
      // This avoids "accidental cache hit" if a file becomes unreadable.
      *hash ^= 0xff;
      *hash = hash.wrapping_mul(fnv_prime);
      crate::instrumentation::record_hash_input_bytes(1);
      return;
    };

    let mut buffer = [0; 8192];
    while let Ok(n) = file.read(&mut buffer) {
      if n == 0 {
        break;
      }
      crate::instrumentation::record_hash_input_bytes(n);
      crate::instrumentation::record_hashed_file_bytes_read(n);
      for byte in &buffer[..n] {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(fnv_prime);
      }
    }
  }

  // Hash workspace root Cargo.toml and Cargo.lock (if present)
  hash_file(&mut hash, &workspace_root.join("Cargo.toml"), FNV_PRIME);
  hash_file(&mut hash, &workspace_root.join("Cargo.lock"), FNV_PRIME);

  // Hash all workspace member manifests (sorted for determinism)
  let mut member_manifests: Vec<PathBuf> = metadata
    .workspace_packages()
    .iter()
    .map(|p| p.manifest_path.as_std_path().to_path_buf())
    .collect();

  member_manifests.sort_unstable_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));

  for manifest_path in member_manifests {
    hash_file(&mut hash, &manifest_path, FNV_PRIME);
  }

  hash
}

// Git State (merged from git_state.rs)

/// Git state for the workspace
///
/// Provides git operations scoped to the workspace repository.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct GitState {
  /// Underlying git backend
  git: SystemGit,

  /// Cached repository root (git working tree)
  repo_root: PathBuf,
}

impl GitState {
  /// Open git repository at the given path
  fn open(path: &Path) -> RailResult<Self> {
    let git = SystemGit::open(path)?;
    let repo_root = git.worktree_root.clone();

    Ok(Self { git, repo_root })
  }

  /// Get repository root path
  pub fn repo_root(&self) -> &Path {
    &self.repo_root
  }

  /// Access underlying SystemGit for advanced operations
  ///
  /// Use this when you need direct access to SystemGit methods.
  pub fn git(&self) -> &SystemGit {
    &self.git
  }

  /// Get current branch name
  ///
  /// Returns "HEAD" if in detached HEAD state.
  pub fn current_branch(&self) -> RailResult<String> {
    self.git.current_branch()
  }

  /// Check if HEAD is detached (not on any branch)
  pub fn is_detached_head(&self) -> RailResult<bool> {
    self.git.is_detached_head()
  }

  /// Get the default branch name (main/master) via remote HEAD
  ///
  /// Returns `None` if no default branch can be determined.
  pub fn default_branch(&self) -> RailResult<Option<String>> {
    self.git.default_branch()
  }
}

// Workspace Context

/// Unified workspace context containing all shared workspace-level data.
///
/// Built once at startup, passed by reference to all commands and operations.
/// This eliminates redundant loads and provides a single source of truth for
/// workspace state.
///
/// Uses Arc for efficient sharing across threads and parallel operations.
/// Cloning is extremely cheap (just increments Arc refcounts).
pub struct WorkspaceContext {
  /// Cargo workspace root (Cargo.toml location)
  /// Note: In most cases, git repo root == workspace root. Access git root via `ctx.git()?.repo_root()` if needed.
  pub workspace_root: PathBuf,

  /// Git state and operations, when the workspace is inside a Git repository.
  ///
  /// Commands that require Git must call [`WorkspaceContext::git`] so Cargo-only
  /// commands can still run in sandboxed workspaces.
  git: Option<Arc<GitState>>,

  /// Cargo metadata and workspace info (Arc for cheap cloning across threads)
  pub cargo: Arc<CargoState>,

  /// Immutable source captured before Cargo metadata can create generated state.
  source_capture: Option<Arc<GitWorktreeCapture>>,

  /// Dependency graph (built from cargo metadata)
  /// Wrapped in Arc for efficient sharing across threads/commands
  pub graph: Arc<WorkspaceGraph>,

  /// Rail configuration (rail.toml)
  /// Optional because not all commands require configuration
  /// Wrapped in Arc for efficient sharing
  pub config: Option<Arc<RailConfig>>,

  /// Configured targets for lazy multi-target metadata loading
  targets: Vec<String>,

  /// Lazy-loaded multi-target metadata (only loaded when unify needs it)
  /// This saves 150-600ms for commands that don't need multi-target analysis.
  /// Uses Mutex<Option<>> for lazy initialization with error handling.
  multi_target_metadata: Mutex<Option<Arc<MultiTargetMetadata>>>,

  /// Lazy rustc cfg sets shared by target-aware analyzers.
  target_cfg_sets: Mutex<Option<Arc<std::collections::HashMap<String, TargetCfgSet>>>>,
}

impl WorkspaceContext {
  /// Build workspace context from a root directory.
  ///
  /// Loads optional git state, cargo metadata, builds graph, and attempts to load rail.toml config.
  /// Config is optional - commands that require it should check and error.
  ///
  /// # Errors
  ///
  /// Returns [`RailError::Message`] if `cargo metadata` fails (e.g., invalid `Cargo.toml`).
  ///
  /// Returns [`RailError::Config`] if `rail.toml` exists but fails to parse.
  ///
  /// # Performance
  ///
  /// - Git state: <5ms (single git rev-parse)
  /// - Cargo metadata: 50-200ms for large workspaces
  /// - Graph build: 10-50ms
  /// - Config load: <5ms (or None if not found)
  /// - **Total: ~100-300ms** (vs 100-300ms × N commands without context)
  pub fn build(workspace_root: &Path) -> RailResult<Self> {
    Self::build_inner(workspace_root, false)
  }

  /// Build a context and optionally capture WORKTREE source before metadata loading.
  #[doc(hidden)]
  pub fn build_with_source_capture(workspace_root: &Path, capture_source: bool) -> RailResult<Self> {
    Self::build_inner(workspace_root, capture_source)
  }

  fn build_inner(workspace_root: &Path, capture_source: bool) -> RailResult<Self> {
    // Load git state when available. Cargo-only commands such as `unify --check`
    // must work in source sandboxes that intentionally omit `.git`.
    let git = match GitState::open(workspace_root) {
      Ok(git) => Some(Arc::new(git)),
      Err(RailError::Git(GitError::RepoNotFound { .. })) => None,
      Err(err) => return Err(err),
    };

    let source_capture = if capture_source {
      git
        .as_ref()
        .map(|state| GitWorktreeCapture::capture(state.git()))
        .transpose()?
        .map(Arc::new)
    } else {
      None
    };

    // Load cargo state
    let cargo = Arc::new(CargoState::load(workspace_root)?);
    let workspace_root = cargo.workspace_root().to_path_buf();

    // Validate git repo root and workspace_root match (or warn if they differ)
    // Canonicalize both paths to handle Windows short (8.3) vs long path formats
    if let Some(git) = git.as_ref() {
      let git_root_canonical = git
        .repo_root()
        .canonicalize()
        .unwrap_or_else(|_| git.repo_root().to_path_buf());
      let workspace_root_canonical = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.clone());

      if git_root_canonical != workspace_root_canonical {
        crate::warn!(
          "git repo root ({}) differs from Cargo workspace root ({})",
          git.repo_root().display(),
          workspace_root.display()
        );
      }
    }

    // Build dependency graph from already-loaded metadata (avoids 50-200ms reload)
    let graph = Arc::new(WorkspaceGraph::from_metadata(cargo.metadata())?);

    // Load optional config - but ERROR on parse failures
    // If a config file exists but is broken, that's a user error we must report
    let config = match RailConfig::try_load(&workspace_root) {
      ConfigLoadResult::Loaded(cfg) => Some(Arc::new(*cfg)),
      ConfigLoadResult::NotFound => None,
      ConfigLoadResult::ParseError { path, message } => {
        return Err(RailError::Config(ConfigError::ParseError { path, message }));
      }
    };

    // Validate configured targets against rustc's canonical target list
    if let Some(ref cfg) = config
      && !cfg.targets.is_empty()
    {
      crate::targets::validate_targets(&cfg.targets)?;
    }

    // Validate config settings that require workspace context
    if let Some(ref cfg) = config {
      // Validate change-detection glob patterns
      cfg.change_detection.validate().map_err(RailError::Config)?;

      // Validate unify config (e.g., transitive_host path)
      cfg.unify.validate(&workspace_root).map_err(RailError::Config)?;

      // Validate run profile schema.
      cfg.run.validate().map_err(RailError::Config)?;
    }

    // Store targets for lazy multi-target metadata loading
    // The metadata is only loaded when unify actually needs it (saves 150-600ms for other commands)
    let targets = config.as_ref().map(|c| c.targets.clone()).unwrap_or_default();

    Ok(Self {
      workspace_root,
      git,
      cargo,
      source_capture,
      graph,
      config,
      targets,
      multi_target_metadata: Mutex::new(None),
      target_cfg_sets: Mutex::new(None),
    })
  }

  /// Get config or error if not found.
  ///
  /// Use this in commands that require rail.toml configuration.
  pub fn require_config(&self) -> RailResult<&Arc<RailConfig>> {
    self.config.as_ref().ok_or_else(|| {
      crate::error::RailError::message(format!(
        "No rail.toml found in: {}\nSearched: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml\nRun 'cargo rail init' to create one.",
        self.workspace_root.display()
      ))
    })
  }

  /// Return Git state for commands that require repository history.
  pub fn git(&self) -> RailResult<&GitState> {
    self.git.as_deref().ok_or_else(|| {
      RailError::Git(GitError::RepoNotFound {
        path: self.workspace_root.clone(),
      })
    })
  }

  /// Return true when this workspace is inside a Git repository.
  pub fn has_git(&self) -> bool {
    self.git.is_some()
  }

  /// Return source captured before metadata loading, when requested by dispatch.
  pub fn source_capture(&self) -> Option<&GitWorktreeCapture> {
    self.source_capture.as_deref()
  }

  /// Get multi-target metadata, loading lazily on first access.
  ///
  /// This is only used by `cargo rail unify`. Other commands don't need it,
  /// so lazy loading saves 150-600ms for commands like `plan`, `run`, `split`, etc.
  ///
  /// The metadata is cached after first load - subsequent calls return the cached value.
  pub fn multi_target_metadata(&self) -> RailResult<Arc<MultiTargetMetadata>> {
    // Lock briefly to check if already loaded
    let mut guard = self
      .multi_target_metadata
      .lock()
      .map_err(|_| RailError::message("Lock poisoned".to_string()))?;

    if let Some(ref cached) = *guard {
      return Ok(Arc::clone(cached));
    }

    // Not loaded yet - load now
    if !self.targets.is_empty() {
      progress!("Loading metadata for {} target(s)...", self.targets.len());
    }
    let metadata = Arc::new(MultiTargetMetadata::load_parallel(
      self.cargo.shared_metadata(),
      &self.targets,
    )?);
    *guard = Some(Arc::clone(&metadata));
    Ok(metadata)
  }

  /// Load each target's exact rustc cfg set once for the command context.
  pub fn target_cfg_sets(&self) -> RailResult<Arc<std::collections::HashMap<String, TargetCfgSet>>> {
    let mut guard = self
      .target_cfg_sets
      .lock()
      .map_err(|_| RailError::message("target cfg cache lock poisoned".to_string()))?;
    if let Some(cached) = guard.as_ref() {
      return Ok(Arc::clone(cached));
    }
    let mut targets = Vec::with_capacity(self.targets.len() + 1);
    targets.push("default");
    targets.extend(self.targets.iter().map(String::as_str));
    let cfg_sets = Arc::new(load_target_cfg_sets(&self.workspace_root, &targets)?);
    *guard = Some(Arc::clone(&cfg_sets));
    Ok(cfg_sets)
  }

  /// Get workspace root as Path reference (convenience)
  pub fn workspace_root(&self) -> &Path {
    &self.workspace_root
  }

  /// Get the relative path from git root to workspace root.
  ///
  /// Returns `Some(prefix)` if workspace is nested inside git repo (e.g., "codex-rs"),
  /// or `None` if they're the same directory.
  ///
  /// This is used to strip the prefix from git-relative paths so they can be
  /// matched against workspace-relative paths (e.g., for crate membership).
  pub fn workspace_prefix(&self) -> Option<PathBuf> {
    let git_root = self.git.as_ref()?.repo_root();

    // On Windows, paths from different sources may have incompatible representations:
    // - Forward vs backslash separators (C:/foo vs C:\foo)
    // - 8.3 short names vs long names (RUNNER~1 vs runneradmin)
    // - Case differences (on case-insensitive filesystems)
    //
    // We must canonicalize both paths to get a consistent representation.
    // Note: canonicalize() adds \\?\ prefix on Windows, but strip_prefix handles this
    // correctly when both paths have the same prefix.
    let git_root_canonical = git_root.canonicalize().unwrap_or_else(|_| git_root.to_path_buf());
    let workspace_canonical = self
      .workspace_root
      .canonicalize()
      .unwrap_or_else(|_| self.workspace_root.clone());

    // If workspace is nested inside git repo, compute the relative prefix
    if let Ok(prefix) = workspace_canonical.strip_prefix(&git_root_canonical) {
      if prefix.as_os_str().is_empty() {
        None // Same directory
      } else {
        Some(prefix.to_path_buf())
      }
    } else {
      None
    }
  }

  /// Convert a git-relative path to a workspace-relative path.
  ///
  /// If the workspace is nested inside the git repo (e.g., git at `/repo`, workspace at `/repo/rust`),
  /// git returns paths like `rust/src/lib.rs` but the workspace expects `src/lib.rs`.
  ///
  /// Returns `None` if the path doesn't belong to this workspace.
  pub fn to_workspace_path(&self, git_path: &Path) -> Option<PathBuf> {
    if let Some(prefix) = self.workspace_prefix() {
      // Git always uses forward slashes, but PathBuf::from converts to platform separators.
      // On Windows, this causes strip_prefix to fail when git_path has / but prefix has \.
      // Normalize git_path by rebuilding through components to use platform separators.
      let normalized = git_path.components().collect::<PathBuf>();
      normalized.strip_prefix(&prefix).ok().map(|p| p.to_path_buf())
    } else {
      Some(git_path.to_path_buf())
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_workspace_context_build() {
    // Use current directory as test workspace
    let current_dir = std::env::current_dir().unwrap();

    // Should successfully build workspace context
    let ctx = WorkspaceContext::build(&current_dir);
    assert!(ctx.is_ok(), "Should successfully build workspace context");

    let ctx = ctx.unwrap();

    // Verify all components are initialized
    assert!(ctx.git().unwrap().repo_root().exists(), "Repo root should exist");
    assert!(ctx.workspace_root.exists(), "Workspace root should exist");

    // Git state should be initialized (git root typically == workspace root)
    // We allow them to differ but in most cases they're the same
    let _ = ctx.git().unwrap().repo_root();

    // Cargo state should be initialized
    assert_eq!(
      ctx.cargo.workspace_root(),
      &ctx.workspace_root,
      "Cargo workspace root should match"
    );

    // Should find cargo-rail in workspace
    let packages = ctx.cargo.workspace_members();
    assert!(!packages.is_empty(), "Should have workspace packages");
    assert!(
      packages.iter().any(|p| p.name == "cargo-rail"),
      "Should find cargo-rail package"
    );

    // Graph should be initialized
    let members = ctx.graph.workspace_members();
    assert!(!members.is_empty(), "Graph should have workspace members");
    assert!(
      members.contains(&"cargo-rail".to_string()),
      "Graph should contain cargo-rail"
    );

    // Config may or may not be loaded depending on workspace
    // Just verify it's an Option
    let _ = ctx.config.as_ref();
  }

  #[test]
  fn test_git_state_wrapper() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Should be able to access git operations via git() accessor
    let head = ctx.git().unwrap().git().head_commit();
    assert!(head.is_ok(), "Should get HEAD commit");

    let head_sha = head.unwrap();
    assert_eq!(head_sha.len(), 40, "HEAD SHA should be 40 characters");

    // Should be able to get current branch
    let branch = ctx.git().unwrap().git().current_branch();
    assert!(branch.is_ok(), "Should get current branch");
  }

  #[test]
  fn test_cargo_state_wrapper() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Should be able to access cargo metadata
    let metadata = ctx.cargo.metadata();
    let packages = metadata.workspace_packages();
    assert!(!packages.is_empty(), "Should have packages");

    // Should be able to get specific package
    let cargo_rail = ctx.cargo.get_package("cargo-rail");
    assert!(cargo_rail.is_some(), "Should find cargo-rail package");

    let pkg = cargo_rail.unwrap();
    assert_eq!(pkg.name.as_str(), "cargo-rail");
  }

  #[test]
  fn test_graph_integration() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Graph should be consistent with cargo metadata
    let graph_members = ctx.graph.workspace_members();
    let cargo_packages: Vec<_> = ctx.cargo.workspace_members().iter().map(|p| p.name.as_str()).collect();

    for member in graph_members {
      assert!(
        cargo_packages.contains(&member.as_str()),
        "Graph member {} should be in cargo packages",
        member
      );
    }
  }
}
