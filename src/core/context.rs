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

use crate::cargo::metadata::WorkspaceMetadata;
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use crate::graph::workspace_graph::WorkspaceGraph;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Unified workspace context containing all shared workspace-level data.
///
/// Built once at startup, passed by reference to all commands and operations.
/// This eliminates redundant loads and provides a single source of truth for
/// workspace state.
///
/// Uses Arc for efficient sharing of graph data without expensive clones.
#[derive(Clone)]
pub struct WorkspaceContext {
  /// Workspace root directory (absolute path)
  pub root: PathBuf,

  /// Cargo metadata (workspace members, dependencies, etc.)
  pub metadata: WorkspaceMetadata,

  /// Dependency graph (built from metadata)
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
  /// Loads metadata, builds graph, and attempts to load rail.toml config.
  /// Config is optional - commands that require it should check and error.
  ///
  /// # Performance
  ///
  /// - Metadata load: 50-200ms for large workspaces
  /// - Graph build: 10-50ms
  /// - Config load: <5ms (or None if not found)
  /// - **Total: ~100-300ms** (vs 100-300ms × N commands without context)
  pub fn build(workspace_root: &Path) -> RailResult<Self> {
    let root = workspace_root.to_path_buf();
    let metadata = WorkspaceMetadata::load(&root)?;
    let graph = Arc::new(WorkspaceGraph::load(&root)?);
    let config = RailConfig::load(&root).ok().map(Arc::new); // Optional - not all commands need it

    Ok(Self {
      root,
      metadata,
      graph,
      config,
    })
  }

  /// Get config or error if not found.
  ///
  /// Use this in commands that require rail.toml configuration.
  pub fn require_config(&self) -> RailResult<&Arc<RailConfig>> {
    self
      .config
      .as_ref()
      .ok_or_else(|| crate::core::error::RailError::message("No rail.toml found. Run 'cargo rail init' to create one."))
  }

  /// Get workspace root as Path reference (convenience)
  pub fn workspace_root(&self) -> &Path {
    &self.root
  }
}
