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
use crate::config::{ConfigLoadResult, RailConfig};
use crate::error::{ConfigError, RailError, RailResult};
use crate::git::SystemGit;
use crate::graph::WorkspaceGraph;
use crate::progress;
use cargo_metadata::{Metadata, MetadataCommand, Package};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ============================================================================
// Cargo State (merged from cargo_state.rs)
// ============================================================================

/// Cargo state for the workspace
///
/// Provides cargo metadata and workspace information.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct CargoState {
  /// Underlying cargo metadata
  metadata: Metadata,

  /// Cached workspace root
  workspace_root: PathBuf,

  /// Index: package name → index in metadata.packages (workspace members only)
  /// Enables O(1) package lookup instead of O(n) linear scan
  package_index: std::collections::HashMap<String, usize>,

  /// Cached set of proc-macro crate names for O(1) detection
  /// Built once during construction, used by change analysis
  proc_macro_crates: std::collections::HashSet<String>,
}

/// Metadata cache structure
#[derive(Serialize, Deserialize)]
struct MetadataCache {
  /// FNV-1a hash of Cargo.toml + Cargo.lock
  hash: u64,
  /// Cached metadata
  metadata: Metadata,
}

impl CargoState {
  /// Load cargo metadata from workspace root
  ///
  /// Uses a content-based cache (FNV-1a hash of Cargo.toml + Cargo.lock)
  /// stored in `target/rail/metadata.json` to speed up subsequent loads.
  fn load(workspace_root: &Path) -> RailResult<Self> {
    let cache_dir = workspace_root.join("target").join("cargo-rail");
    let cache_file = cache_dir.join("metadata.json");

    // Compute current hash
    let current_hash = compute_workspace_hash(workspace_root);

    // Try to load from cache
    // Cache is valid only if:
    // 1. Hash matches (Cargo.toml + Cargo.lock unchanged)
    // 2. Workspace root path matches (repo hasn't been moved/copied)
    if let Some(cache) = fs::read_to_string(&cache_file)
      .ok()
      .and_then(|s| serde_json::from_str::<MetadataCache>(&s).ok())
      && cache.hash == current_hash
    {
      // Validate workspace root path matches current location
      // This handles the case where a repo is copied/moved to a different path
      let cached_root = cache.metadata.workspace_root.as_std_path();
      let current_root_canonical = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
      let cached_root_canonical = cached_root.canonicalize().unwrap_or_else(|_| cached_root.to_path_buf());

      if current_root_canonical == cached_root_canonical {
        // Cache hit - use cached metadata
        return Ok(Self::from_metadata(cache.metadata));
      }
      // Path mismatch - cache is stale, will reload below
    }

    // Cache miss or mismatch - load fresh metadata
    let metadata = MetadataCommand::new()
      .manifest_path(workspace_root.join("Cargo.toml"))
      .exec()?;

    // Save to cache
    if let Ok(()) = fs::create_dir_all(&cache_dir) {
      let cache = MetadataCache {
        hash: current_hash,
        metadata: metadata.clone(),
      };
      let _ = fs::write(&cache_file, serde_json::to_string(&cache).unwrap_or_default());
    }

    Ok(Self::from_metadata(metadata))
  }

  /// Build CargoState from metadata, constructing the package index and proc-macro cache
  fn from_metadata(metadata: Metadata) -> Self {
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();

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

  /// Check if a crate is a proc-macro crate (O(1) lookup)
  pub fn is_proc_macro(&self, crate_name: &str) -> bool {
    self.proc_macro_crates.contains(crate_name)
  }

  /// Get the set of all proc-macro crate names
  pub fn proc_macro_crates(&self) -> &std::collections::HashSet<String> {
    &self.proc_macro_crates
  }
}

/// Compute a content-based hash of workspace manifests
///
/// Uses FNV-1a hash of Cargo.toml + Cargo.lock to detect changes
fn compute_workspace_hash(workspace_root: &Path) -> u64 {
  const FNV_PRIME: u64 = 0x100000001b3;
  const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

  let mut hash = FNV_OFFSET_BASIS;

  // Hash Cargo.toml
  if let Ok(mut file) = fs::File::open(workspace_root.join("Cargo.toml")) {
    let mut buffer = [0; 8192];
    while let Ok(n) = file.read(&mut buffer) {
      if n == 0 {
        break;
      }
      for byte in &buffer[..n] {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
      }
    }
  }

  // Hash Cargo.lock if it exists
  if let Ok(mut file) = fs::File::open(workspace_root.join("Cargo.lock")) {
    let mut buffer = [0; 8192];
    while let Ok(n) = file.read(&mut buffer) {
      if n == 0 {
        break;
      }
      for byte in &buffer[..n] {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
      }
    }
  }

  hash
}

// ============================================================================
// Git State (merged from git_state.rs)
// ============================================================================

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
}

// ============================================================================
// Workspace Context
// ============================================================================

/// Unified workspace context containing all shared workspace-level data.
///
/// Built once at startup, passed by reference to all commands and operations.
/// This eliminates redundant loads and provides a single source of truth for
/// workspace state.
///
/// Uses Arc for efficient sharing across threads and parallel operations.
/// Cloning is extremely cheap (just increments Arc refcounts).
#[derive(Clone)]
pub struct WorkspaceContext {
  /// Cargo workspace root (Cargo.toml location)
  /// Note: In most cases, git repo root == workspace root. Access git root via ctx.git.repo_root() if needed.
  pub workspace_root: PathBuf,

  /// Git state and operations (Arc for cheap cloning across threads)
  pub git: Arc<GitState>,

  /// Cargo metadata and workspace info (Arc for cheap cloning across threads)
  pub cargo: Arc<CargoState>,

  /// Dependency graph (built from cargo metadata)
  /// Wrapped in Arc for efficient sharing across threads/commands
  pub graph: Arc<WorkspaceGraph>,

  /// Rail configuration (rail.toml)
  /// Optional because not all commands require configuration
  /// Wrapped in Arc for efficient sharing
  pub config: Option<Arc<RailConfig>>,

  /// Multi-target metadata (pre-loaded if targets configured)
  /// This saves 150-600ms for unify operations by loading once
  pub multi_target_metadata: Option<Arc<MultiTargetMetadata>>,
}

impl WorkspaceContext {
  /// Build workspace context from a root directory.
  ///
  /// Loads git state, cargo metadata, builds graph, and attempts to load rail.toml config.
  /// Config is optional - commands that require it should check and error.
  ///
  /// # Performance
  ///
  /// - Git state: <5ms (single git rev-parse)
  /// - Cargo metadata: 50-200ms for large workspaces
  /// - Graph build: 10-50ms
  /// - Config load: <5ms (or None if not found)
  /// - **Total: ~100-300ms** (vs 100-300ms × N commands without context)
  pub fn build(workspace_root: &Path) -> RailResult<Self> {
    // Load git state
    let git = Arc::new(GitState::open(workspace_root)?);

    // Load cargo state
    let cargo = Arc::new(CargoState::load(workspace_root)?);
    let workspace_root = cargo.workspace_root().to_path_buf();

    // Validate git repo root and workspace_root match (or warn if they differ)
    // Canonicalize both paths to handle Windows short (8.3) vs long path formats
    let git_root_canonical = git
      .repo_root()
      .canonicalize()
      .unwrap_or_else(|_| git.repo_root().to_path_buf());
    let workspace_root_canonical = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.clone());

    if git_root_canonical != workspace_root_canonical {
      eprintln!(
        "⚠️  Warning: Git repo root ({}) differs from Cargo workspace root ({})",
        git.repo_root().display(),
        workspace_root.display()
      );
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
    }

    // Pre-load multi-target metadata if targets are configured
    // This saves 150-600ms for unify operations
    let multi_target_metadata = if let Some(ref cfg) = config
      && !cfg.targets.is_empty()
    {
      progress!("Pre-loading metadata for {} target(s)...", cfg.targets.len());
      Some(Arc::new(MultiTargetMetadata::load_parallel(
        &workspace_root,
        &cfg.targets,
      )?))
    } else {
      None
    };

    Ok(Self {
      workspace_root,
      git,
      cargo,
      graph,
      config,
      multi_target_metadata,
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

  /// Get rail config (convenience wrapper for require_config)
  pub fn rail_config(&self) -> RailResult<&RailConfig> {
    self.require_config().map(|arc| arc.as_ref())
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
    let git_root = self.git.repo_root();

    // If workspace is nested inside git repo, compute the relative prefix
    if let Ok(prefix) = self.workspace_root.strip_prefix(git_root) {
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
      git_path.strip_prefix(&prefix).ok().map(|p| p.to_path_buf())
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
    assert!(ctx.git.repo_root().exists(), "Repo root should exist");
    assert!(ctx.workspace_root.exists(), "Workspace root should exist");

    // Git state should be initialized (git root typically == workspace root)
    // We allow them to differ but in most cases they're the same
    let _ = ctx.git.repo_root();

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
    let head = ctx.git.git().head_commit();
    assert!(head.is_ok(), "Should get HEAD commit");

    let head_sha = head.unwrap();
    assert_eq!(head_sha.len(), 40, "HEAD SHA should be 40 characters");

    // Should be able to get current branch
    let branch = ctx.git.git().current_branch();
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
