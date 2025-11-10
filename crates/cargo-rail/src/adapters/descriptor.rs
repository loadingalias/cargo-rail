//! Package descriptor trait - language-agnostic package abstraction
//!
//! This module provides a clean interface for package-level operations that
//! work across different language ecosystems (Rust, JavaScript, Python, etc.).
//!
//! The `PackageDescriptor` trait is complementary to `LanguageAdapter`:
//! - `LanguageAdapter`: Workspace-level operations (discovery, batch transforms)
//! - `PackageDescriptor`: Package-level operations (name, version, dependencies)
//!
//! This separation makes it easier to:
//! 1. Add new language support (just implement both traits)
//! 2. Keep core logic language-agnostic
//! 3. Support polyglot workspaces in the future

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Generic dependency information for a package
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
  /// Dependency name
  pub name: String,
  /// Dependency specification (version, path, git, etc.)
  pub spec: DependencySpec,
  /// Whether this is a dev dependency
  pub is_dev: bool,
  /// Whether this is a build dependency
  pub is_build: bool,
}

/// Dependency specification type
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySpec {
  /// Version constraint (e.g., "1.0.0", "^1.0", "~1.2.3")
  Version(String),
  /// Path dependency (e.g., "../other-package")
  Path(PathBuf),
  /// Git dependency with optional revision
  Git { url: String, rev: Option<String> },
  /// Workspace dependency (inherits from workspace)
  Workspace,
}

/// Package descriptor trait - represents a single package/crate
///
/// Each language ecosystem implements this trait to provide package-level
/// metadata and operations. The trait is designed to be minimal but sufficient
/// for core split/sync operations.
///
/// # Example
///
/// ```rust,ignore
/// use cargo_rail::adapters::descriptor::{PackageDescriptor, DependencySpec};
///
/// // For a Rust crate
/// let pkg = CargoPackageDescriptor::from_path("crates/my-crate")?;
/// println!("Name: {}", pkg.name());
/// println!("Version: {}", pkg.version());
///
/// // For a Node.js package
/// let pkg = NodePackageDescriptor::from_path("packages/my-package")?;
/// println!("Name: {}", pkg.name());
/// ```
pub trait PackageDescriptor: Send + Sync {
  /// Get the package name
  fn name(&self) -> &str;

  /// Get the current version
  fn version(&self) -> &str;

  /// Get the package description (if any)
  fn description(&self) -> Option<&str>;

  /// Get the list of dependencies
  fn dependencies(&self) -> &[Dependency];

  /// Get the path to the package root directory
  fn path(&self) -> &Path;

  /// Get the path to the manifest file (e.g., Cargo.toml, package.json)
  fn manifest_path(&self) -> PathBuf {
    self.path().join(self.manifest_filename())
  }

  /// Get the manifest filename (e.g., "Cargo.toml", "package.json")
  fn manifest_filename(&self) -> &str;

  /// Update the version in the manifest file
  ///
  /// This modifies the manifest file on disk.
  fn update_version(&self, new_version: &str) -> Result<()>;

  /// Check if this package depends on another package in the workspace
  fn depends_on(&self, other_name: &str) -> bool {
    self.dependencies().iter().any(|dep| dep.name == other_name)
  }

  /// Get workspace dependencies (dependencies that use workspace inheritance)
  fn workspace_dependencies(&self) -> Vec<&Dependency> {
    self
      .dependencies()
      .iter()
      .filter(|dep| matches!(dep.spec, DependencySpec::Workspace))
      .collect()
  }

  /// Get path dependencies
  fn path_dependencies(&self) -> Vec<&Dependency> {
    self
      .dependencies()
      .iter()
      .filter(|dep| matches!(dep.spec, DependencySpec::Path(_)))
      .collect()
  }
}

/// Helper to create a PackageDescriptor from a path
///
/// This will auto-detect the language ecosystem and return the appropriate
/// descriptor implementation.
pub fn from_path(path: &Path) -> Result<Box<dyn PackageDescriptor>> {
  // Try Cargo first
  if path.join("Cargo.toml").exists() {
    return crate::adapters::cargo::create_package_descriptor(path);
  }

  // Try Node.js (future)
  // if path.join("package.json").exists() {
  //   return crate::adapters::node::create_package_descriptor(path);
  // }

  // Try Python (future)
  // if path.join("pyproject.toml").exists() {
  //   return crate::adapters::python::create_package_descriptor(path);
  // }

  anyhow::bail!(
    "Could not detect package type at {}\n\
     Supported: Cargo (Cargo.toml)",
    path.display()
  )
}
