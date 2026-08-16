//! File I/O operations for TOML manifests

use crate::error::{RailResult, ResultExt};
use std::path::Path;
use toml_edit::DocumentMut;

/// Read and parse a TOML file into a DocumentMut
///
/// Provides consistent error handling for reading and parsing Cargo.toml files.
///
/// # Errors
///
/// Returns error if file cannot be read or TOML is invalid
pub fn read_toml_file(path: &Path) -> RailResult<DocumentMut> {
  let content = std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

  content
    .parse()
    .with_context(|| format!("Failed to parse {}", path.display()))
}

/// Write a TOML document to a file
///
/// Provides consistent error handling for writing Cargo.toml files.
///
/// # Errors
///
/// Returns error if file cannot be written
pub fn write_toml_file(path: &Path, doc: &DocumentMut) -> RailResult<()> {
  crate::utils::write_file_atomic(path, doc.to_string().as_bytes())
    .with_context(|| format!("Failed to write {}", path.display()))
}
