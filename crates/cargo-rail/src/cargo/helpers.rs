//! Cargo-specific helper functions for workspace operations

use crate::core::error::RailResult;
use std::path::{Path, PathBuf};

use super::files::AuxiliaryFiles;

/// Check if a path should be excluded from operations (Cargo-specific)
pub fn should_exclude_cargo_path(path: &Path) -> bool {
  if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
    matches!(name, "target" | ".git" | "Cargo.lock" | ".DS_Store")
  } else {
    false
  }
}

/// Discover auxiliary files for a Cargo package
pub fn discover_aux_files(package_path: &Path) -> RailResult<Vec<PathBuf>> {
  let aux = AuxiliaryFiles::discover(package_path)?;
  Ok(aux.list_target_paths())
}
