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

use crate::cargo::WorkspaceMetadata;
use crate::config::RailConfig;
use crate::error::RailResult;
use crate::graph::WorkspaceGraph;
use crate::workspace::{CargoState, GitState};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Unified workspace context containing all shared workspace-level data.
///
/// Built once at startup, passed by reference to all commands and operations.
/// This eliminates redundant loads and provides a single source of truth for
/// workspace state.
///
/// Uses Arc for efficient sharing of graph data without expensive clones.
/// Uses OnceLock for lazy-loaded metadata variants (e.g., --all-features).
#[derive(Clone)]
pub struct WorkspaceContext {
  /// Cargo workspace root (Cargo.toml location)
  /// Note: In most cases, git repo root == workspace root. Access git root via ctx.git.repo_root() if needed.
  pub workspace_root: PathBuf,

  /// Git state and operations
  pub git: GitState,

  /// Cargo metadata and workspace info (default features)
  pub cargo: CargoState,

  /// Cargo metadata with --all-features (lazy-loaded, cached)
  /// Used by unify and other commands that need complete feature resolution.
  /// Loads on first access, then cached for the lifetime of the context.
  cargo_all_features: Arc<OnceLock<WorkspaceMetadata>>,

  /// Dependency graph (built from cargo metadata)
  /// Wrapped in Arc for efficient sharing across threads/commands
  pub graph: Arc<WorkspaceGraph>,

  /// Rail configuration (rail.toml)
  /// Optional because not all commands require configuration
  /// Wrapped in Arc for efficient sharing
  pub config: Option<Arc<RailConfig>>,
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
    let git = GitState::open(workspace_root)?;

    // Load cargo state
    let cargo = CargoState::load(workspace_root)?;
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

    // Build dependency graph
    let graph = Arc::new(WorkspaceGraph::load(&workspace_root)?);

    // Load optional config
    let config = RailConfig::load(&workspace_root).ok().map(Arc::new);

    Ok(Self {
      workspace_root,
      git,
      cargo,
      cargo_all_features: Arc::new(OnceLock::new()),
      graph,
      config,
    })
  }

  /// Get metadata with --all-features (lazy-loaded, cached)
  ///
  /// This is used by unify and other commands that need complete feature resolution
  /// across the workspace. The metadata is loaded once on first access and then
  /// cached for the lifetime of the WorkspaceContext.
  ///
  /// # Performance
  ///
  /// - First call: 50-200ms (runs `cargo metadata --all-features`)
  /// - Subsequent calls: <1ms (returns cached reference)
  ///
  /// # Example
  ///
  /// ```ignore
  /// let metadata = ctx.metadata_all_features()?;
  /// // metadata is &WorkspaceMetadata, no clone needed
  /// ```
  pub fn metadata_all_features(&self) -> RailResult<&WorkspaceMetadata> {
    // Check if already loaded
    if let Some(metadata) = self.cargo_all_features.get() {
      return Ok(metadata);
    }

    // Not loaded yet - load it
    let metadata = WorkspaceMetadata::load_with_features(&self.workspace_root, true, false, vec![])?;

    // Try to store it (may fail if another thread beat us, that's OK)
    match self.cargo_all_features.set(metadata) {
      Ok(()) => {
        // We successfully stored it, return the reference
        Ok(self.cargo_all_features.get().expect("just set"))
      }
      Err(_) => {
        // Another thread beat us to storing - that's fine, return their value
        // (The metadata we loaded will be dropped)
        Ok(self.cargo_all_features.get().expect("another thread set this"))
      }
    }
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
    let packages = ctx.cargo.workspace_packages();
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

    // Should be able to access git operations
    let head = ctx.git.head_commit();
    assert!(head.is_ok(), "Should get HEAD commit");

    let head_sha = head.unwrap();
    assert_eq!(head_sha.len(), 40, "HEAD SHA should be 40 characters");

    // Should be able to get current branch
    let branch = ctx.git.current_branch();
    assert!(branch.is_ok(), "Should get current branch");
  }

  #[test]
  fn test_cargo_state_wrapper() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Should be able to access cargo metadata
    let metadata = ctx.cargo.metadata();
    let packages = metadata.list_crates();
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
    let cargo_packages: Vec<_> = ctx.cargo.workspace_packages().iter().map(|p| p.name.as_str()).collect();

    for member in &graph_members {
      assert!(
        cargo_packages.contains(&member.as_str()),
        "Graph member {} should be in cargo packages",
        member
      );
    }
  }

  #[test]
  fn test_metadata_all_features_lazy_loading() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // First access should load and cache
    let metadata1 = ctx.metadata_all_features();
    assert!(metadata1.is_ok(), "Should successfully load all-features metadata");

    // Second access should return cached reference (same pointer)
    let metadata2 = ctx.metadata_all_features();
    assert!(metadata2.is_ok(), "Should successfully get cached metadata");

    // Verify they're the same reference (pointer equality)
    let ptr1 = metadata1.unwrap() as *const _;
    let ptr2 = metadata2.unwrap() as *const _;
    assert_eq!(ptr1, ptr2, "Should return same cached instance");
  }

  #[test]
  fn test_metadata_all_features_vs_default() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Get default metadata
    let default_metadata = ctx.cargo.metadata();

    // Get all-features metadata
    let all_features_metadata = ctx.metadata_all_features().unwrap();

    // They should be different objects (different pointers)
    let ptr_default = default_metadata as *const _;
    let ptr_all_features = all_features_metadata as *const _;
    assert_ne!(
      ptr_default, ptr_all_features,
      "Default and all-features metadata should be separate instances"
    );

    // Both should have the same workspace root
    assert_eq!(
      default_metadata.workspace_root(),
      all_features_metadata.workspace_root(),
      "Both metadata instances should have same workspace root"
    );
  }
}
