//! Language-specific adapters for workspace management
//!
//! This module provides a clean abstraction for supporting multiple
//! language ecosystems (Rust/Cargo, Node.js/npm, Python/uv, etc.)
//!
//! NOTE: This architecture is designed for future polyglot support (JS/TS, Python, etc.)
//! and is not yet fully utilized in v1.0 (Rust-only).

#![allow(dead_code)]

use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod cargo;

/// Generic workspace information
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
  pub root: PathBuf,
  pub packages: Vec<PackageInfo>,
}

/// Generic package/crate information
#[derive(Debug, Clone)]
pub struct PackageInfo {
  pub name: String,
  pub version: String,
  pub path: PathBuf,
  pub manifest_path: PathBuf,
  pub dependencies: Vec<DependencyInfo>,
}

/// Dependency information
#[derive(Debug, Clone)]
pub struct DependencyInfo {
  pub name: String,
  pub spec: DependencySpec,
}

/// How a dependency is specified
#[derive(Debug, Clone)]
pub enum DependencySpec {
  /// Version constraint (e.g., "1.0.0", "^1.0", "workspace")
  Version(String),
  /// Path dependency (e.g., "../other-crate")
  Path(PathBuf),
  /// Git dependency
  Git { url: String, rev: Option<String> },
}

/// Context for transforming manifests
#[derive(Debug, Clone)]
pub struct TransformContext {
  pub workspace_root: PathBuf,
  pub package_name: String,
  pub target_mode: TransformMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformMode {
  /// Transforming for split: path deps → version deps
  SplitToRemote,
  /// Transforming for sync remote→mono: version deps → path deps
  SyncToMono,
  /// Transforming for sync mono→remote: path deps → version deps
  SyncToRemote,
}

/// Language adapter trait
///
/// Each language ecosystem (Rust, Node.js, Python) implements this trait
/// to provide workspace discovery, manifest transformations, and file operations.
pub trait LanguageAdapter: Send + Sync {
  /// Detect if this adapter can handle the given workspace
  fn can_handle(&self, root: &Path) -> bool;

  /// Load workspace metadata
  fn load_workspace(&self, root: &Path) -> Result<WorkspaceInfo>;

  /// Transform a package manifest for the given mode
  fn transform_manifest(&self, manifest_path: &Path, context: &TransformContext) -> Result<()>;

  /// Discover auxiliary files that should be copied (e.g., rust-toolchain.toml, .nvmrc)
  fn discover_aux_files(&self, package_path: &Path) -> Result<Vec<PathBuf>>;

  /// Check if a path should be excluded from operations (e.g., node_modules, target)
  fn should_exclude(&self, path: &Path) -> bool;

  /// Get the manifest filename (e.g., "Cargo.toml", "package.json")
  fn manifest_filename(&self) -> &str;
}

/// Detect the appropriate language adapter for a workspace
pub fn detect_adapter(root: &Path) -> Result<Box<dyn LanguageAdapter>> {
  let cargo_adapter = cargo::CargoAdapter::new();

  if cargo_adapter.can_handle(root) {
    return Ok(Box::new(cargo_adapter));
  }

  anyhow::bail!(
    "Could not detect language ecosystem for workspace at {}\n\
     Supported: Cargo (Cargo.toml with [workspace])",
    root.display()
  )
}
