//! Workspace path dependency handling

use crate::cargo::WorkspaceMetadata;
use cargo_metadata::camino::Utf8PathBuf;

/// Check if all path dependencies point to the same workspace member
pub fn are_all_identical_workspace_paths(paths: &[(String, Utf8PathBuf)], metadata: &WorkspaceMetadata) -> bool {
  if paths.is_empty() {
    return false;
  }

  // Get the first path and verify it's a workspace member
  let first_path = &paths[0].1;
  if !is_workspace_member_path(first_path, metadata) {
    return false;
  }

  // Normalize the first path for comparison
  let first_normalized = normalize_workspace_path(first_path, metadata);

  // All paths must normalize to the same workspace member
  paths.iter().all(|(_, p)| {
    let normalized = normalize_workspace_path(p, metadata);
    first_normalized == normalized
  })
}

/// Normalize a path to be relative to workspace root
pub fn normalize_workspace_path(path: &Utf8PathBuf, metadata: &WorkspaceMetadata) -> Utf8PathBuf {
  let workspace_root = metadata.workspace_root_utf8();

  // Try to resolve relative paths like "../foo" to workspace-relative "foo"
  if path.starts_with("..") {
    // Handle relative paths - try to match against workspace members
    for pkg in metadata.list_crates() {
      if let Some(pkg_dir) = pkg.manifest_path.parent()
        && let Ok(rel_path) = pkg_dir.strip_prefix(workspace_root)
        && (path.as_str() == format!("../{}", rel_path) || path.as_str() == rel_path.as_str())
      {
        return rel_path.to_path_buf();
      }
    }
  }

  // Already normalized or absolute
  path.clone()
}

/// Check if path points to a workspace member
pub fn is_workspace_member_path(path: &Utf8PathBuf, metadata: &WorkspaceMetadata) -> bool {
  let workspace_root = metadata.workspace_root_utf8();

  for pkg in metadata.list_crates() {
    if let Some(pkg_dir) = pkg.manifest_path.parent() {
      // Normalize and compare paths
      if let Ok(rel_path) = pkg_dir.strip_prefix(workspace_root) {
        // Check if the dependency path matches this member's path
        if path.as_str() == rel_path.as_str() || path.as_str() == format!("../{}", rel_path) {
          return true;
        }
      }

      // Also check absolute path match
      if pkg_dir == path {
        return true;
      }
    }
  }

  false
}
