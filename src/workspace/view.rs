//! Unified view into workspace crate information
//!
//! Provides a high-level façade over `WorkspaceContext`, `WorkspaceGraph`, and `CargoState`.
//! Many operations need "given a crate name, tell me its path, whether it's proc‑macro,
//! what deps it has" - this façade de-duplicates that pattern.
//!
//! # Example
//!
//! ```text
//! let view = WorkspaceView::new(&ctx);
//! if let Some(info) = view.crate_info("my-crate") {
//!     println!("Path: {}", info.crate_root.display());
//!     println!("Is proc-macro: {}", info.is_proc_macro);
//! }
//! ```

use crate::workspace::WorkspaceContext;
use std::path::PathBuf;

/// High-level façade for querying crate information
///
/// Backed by `&WorkspaceContext`, provides convenient access to:
/// - Crate metadata (path, proc-macro status)
/// - Dependency information from the graph
/// - File-to-crate mapping
pub struct WorkspaceView<'a> {
  ctx: &'a WorkspaceContext,
}

/// Information about a single crate in the workspace
#[derive(Debug, Clone)]
pub struct CrateInfo {
  /// Crate name
  pub name: String,
  /// Root directory of the crate (parent of Cargo.toml)
  pub crate_root: PathBuf,
  /// Path to Cargo.toml
  pub manifest_path: PathBuf,
  /// Whether this crate is a proc-macro crate
  pub is_proc_macro: bool,
  /// Current version from Cargo.toml
  pub version: semver::Version,
}

impl<'a> WorkspaceView<'a> {
  /// Create a new WorkspaceView backed by the given context
  pub fn new(ctx: &'a WorkspaceContext) -> Self {
    Self { ctx }
  }

  /// Get information about a specific crate by name
  ///
  /// Returns `None` if the crate is not a workspace member.
  pub fn crate_info(&self, crate_name: &str) -> Option<CrateInfo> {
    let package = self.ctx.cargo.get_package(crate_name)?;
    let manifest_path = package.manifest_path.clone().into_std_path_buf();
    let crate_root = manifest_path.parent()?.to_path_buf();

    Some(CrateInfo {
      name: crate_name.to_string(),
      crate_root,
      manifest_path,
      is_proc_macro: self.ctx.cargo.is_proc_macro(crate_name),
      version: package.version.clone(),
    })
  }

  /// Get all crate infos for workspace members
  pub fn all_crates(&self) -> Vec<CrateInfo> {
    self
      .ctx
      .graph
      .workspace_members()
      .iter()
      .filter_map(|name| self.crate_info(name))
      .collect()
  }

  /// Get transitive dependents of a crate (crates that depend on it)
  pub fn dependents(&self, crate_name: &str) -> crate::error::RailResult<Vec<String>> {
    self.ctx.graph.transitive_dependents(crate_name)
  }

  /// Map a file path to its owning crate name
  pub fn file_to_crate(&self, path: &std::path::Path) -> Option<String> {
    self.ctx.graph.file_to_crate(path)
  }

  /// Check if a crate is a proc-macro crate (O(1))
  pub fn is_proc_macro(&self, crate_name: &str) -> bool {
    self.ctx.cargo.is_proc_macro(crate_name)
  }

  /// Get all proc-macro crate names
  pub fn proc_macro_crates(&self) -> &std::collections::HashSet<String> {
    self.ctx.cargo.proc_macro_crates()
  }

  /// Get workspace members in dependency order (for publishing)
  pub fn publish_order(&self) -> crate::error::RailResult<Vec<String>> {
    self.ctx.graph.publish_order()
  }

  /// Access the underlying context (for advanced use cases)
  pub fn context(&self) -> &WorkspaceContext {
    self.ctx
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn create_test_context() -> WorkspaceContext {
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceContext::build(&current_dir).unwrap()
  }

  #[test]
  fn test_workspace_view_crate_info() {
    let ctx = create_test_context();
    let view = WorkspaceView::new(&ctx);

    // cargo-rail should be found
    let info = view.crate_info("cargo-rail");
    assert!(info.is_some(), "Should find cargo-rail");

    let info = info.unwrap();
    assert_eq!(info.name, "cargo-rail");
    assert!(info.crate_root.exists(), "Crate root should exist");
    assert!(info.manifest_path.exists(), "Manifest should exist");
    assert!(!info.is_proc_macro, "cargo-rail is not a proc-macro");
  }

  #[test]
  fn test_workspace_view_all_crates() {
    let ctx = create_test_context();
    let view = WorkspaceView::new(&ctx);

    let all = view.all_crates();
    assert!(!all.is_empty(), "Should have at least one crate");
    assert!(all.iter().any(|c| c.name == "cargo-rail"), "Should include cargo-rail");
  }

  #[test]
  fn test_workspace_view_proc_macro() {
    let ctx = create_test_context();
    let view = WorkspaceView::new(&ctx);

    // cargo-rail is not a proc-macro
    assert!(!view.is_proc_macro("cargo-rail"));

    // The set should be accessible
    let _proc_macros = view.proc_macro_crates();
  }
}
