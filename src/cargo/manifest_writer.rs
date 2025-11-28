//! Clean manifest writing operations
//!
//! Handles writing [workspace.dependencies] and updating member manifests

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::manifest_ops;
use crate::cargo::unify_analyzer::{TransitivePin, UnifiedDep};
use crate::error::{RailResult, ResultExt};
use crate::toml::format::TomlFormatter;
use std::path::Path;

/// Writes changes to Cargo.toml files
pub struct ManifestWriter {
  formatter: TomlFormatter,
}

impl Default for ManifestWriter {
  fn default() -> Self {
    Self::new()
  }
}

impl ManifestWriter {
  /// Creates a new manifest writer
  pub fn new() -> Self {
    Self {
      formatter: TomlFormatter::new(),
    }
  }

  /// Write unified dependencies to workspace Cargo.toml
  ///
  /// IMPORTANT: This MERGES new deps with existing workspace.dependencies.
  /// It does NOT replace the entire section.
  pub fn write_workspace_deps(&self, workspace_toml_path: &Path, deps: &[UnifiedDep]) -> RailResult<()> {
    // Read workspace Cargo.toml
    let mut doc = manifest_ops::read_toml_file(workspace_toml_path)?;

    // Ensure [workspace] section exists
    manifest_ops::ensure_section(&mut doc, "workspace").context("Failed to create [workspace] section")?;

    // Get or create [workspace.dependencies] - DO NOT CLEAR IT
    // We merge new deps with existing ones, not replace them
    let deps_table = manifest_ops::get_or_create_table(&mut doc, "workspace.dependencies")
      .context("Failed to create [workspace.dependencies]")?;
    // NOTE: Removed deps_table.clear() - BUG FIX: preserve existing deps

    // Group dependencies by target
    let (regular_deps, target_deps) = self.group_dependencies(deps);

    // Write regular dependencies
    for dep in regular_deps {
      let entry = manifest_ops::build_dep_entry(&dep);
      manifest_ops::insert_dependency(deps_table, &dep.name, entry).context("Failed to insert regular dependency")?;
    }

    // Write target-specific dependencies
    for (target, deps) in target_deps {
      for dep in deps {
        let entry = manifest_ops::build_dep_entry(&dep);
        manifest_ops::insert_target_dependency(&mut doc, &target, "dependencies", &dep.name, entry)
          .context("Failed to insert target dependency")?;
      }
    }

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(workspace_toml_path, &doc)?;

    Ok(())
  }

  /// Group dependencies into regular and target-specific
  fn group_dependencies(
    &self,
    deps: &[UnifiedDep],
  ) -> (Vec<UnifiedDep>, std::collections::HashMap<String, Vec<UnifiedDep>>) {
    let mut regular_deps = Vec::new();
    let mut target_deps: std::collections::HashMap<String, Vec<UnifiedDep>> = std::collections::HashMap::new();

    for dep in deps {
      if let Some(ref target) = dep.target {
        target_deps.entry(target.clone()).or_default().push(dep.clone());
      } else {
        regular_deps.push(dep.clone());
      }
    }

    (regular_deps, target_deps)
  }

  /// Update a member's Cargo.toml to use workspace inheritance
  ///
  /// # Arguments
  ///
  /// * `member_toml_path` - Path to the member's Cargo.toml
  /// * `dep_name` - Name of the dependency to update
  /// * `dep_kind` - Type of dependency (Normal, Dev, Build)
  /// * `target` - Optional target platform constraint (e.g., "cfg(unix)")
  /// * `local_features` - Additional features to enable locally
  /// * `is_optional` - Whether the dependency is optional
  pub fn update_member(
    &self,
    member_toml_path: &Path,
    dep_name: &str,
    dep_kind: DepKind,
    target: Option<&str>,
    local_features: Option<Vec<String>>,
    is_optional: bool,
  ) -> RailResult<()> {
    // Read member Cargo.toml
    let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

    // Get section name from kind
    let kind_section = self.dep_kind_to_section(dep_kind);

    // Build workspace-inherited entry
    let entry = manifest_ops::build_workspace_dep_entry(local_features, is_optional);

    // Handle target-specific vs regular sections
    if let Some(target_cfg) = target {
      // Target-specific: write to [target.'cfg(...)'.dependencies]
      manifest_ops::insert_target_dependency(&mut doc, target_cfg, kind_section, dep_name, entry)
        .context("Failed to insert target-specific workspace dependency")?;
    } else {
      // Regular section: write to [dependencies], [dev-dependencies], or [build-dependencies]
      let deps =
        manifest_ops::get_or_create_table(&mut doc, kind_section).context("Failed to get dependencies section")?;
      manifest_ops::insert_dependency(deps, dep_name, entry).context("Failed to insert workspace dependency")?;
    }

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(member_toml_path, &doc)?;

    Ok(())
  }

  /// Add transitive dependencies for pinning (workspace-hack replacement)
  ///
  /// This adds entries with `workspace = true` to the host's dev-dependencies.
  /// IMPORTANT: The caller must ensure these deps are already in [workspace.dependencies]
  /// before calling this function. Use `write_transitive_workspace_deps` first.
  pub fn add_transitive_pins(&self, host_toml_path: &Path, transitives: &[TransitivePin]) -> RailResult<()> {
    // Read host Cargo.toml (usually workspace root)
    let mut doc = manifest_ops::read_toml_file(host_toml_path)?;

    // Ensure [dev-dependencies] exists
    let dev_deps =
      manifest_ops::get_or_create_table(&mut doc, "dev-dependencies").context("Failed to create [dev-dependencies]")?;

    // Add each transitive as a dev dependency with workspace = true
    for pin in transitives {
      let entry = manifest_ops::build_transitive_entry(&pin.features);
      manifest_ops::insert_dependency(dev_deps, &pin.name, entry).context("Failed to insert transitive dependency")?;
    }

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(host_toml_path, &doc)?;

    Ok(())
  }

  /// Write transitive dependencies to [workspace.dependencies]
  ///
  /// This must be called BEFORE `add_transitive_pins` so that the deps exist
  /// in workspace.dependencies when referenced with `workspace = true`.
  pub fn write_transitive_workspace_deps(
    &self,
    workspace_toml_path: &Path,
    transitives: &[TransitivePin],
  ) -> RailResult<()> {
    // Read workspace Cargo.toml
    let mut doc = manifest_ops::read_toml_file(workspace_toml_path)?;

    // Ensure [workspace.dependencies] exists
    manifest_ops::ensure_section(&mut doc, "workspace").context("Failed to create [workspace] section")?;
    let deps_table = manifest_ops::get_or_create_table(&mut doc, "workspace.dependencies")
      .context("Failed to create [workspace.dependencies]")?;

    // Add each transitive dependency with version and features
    for pin in transitives {
      let entry = manifest_ops::build_versioned_dep_entry(&pin.version, &pin.features);
      manifest_ops::insert_dependency(deps_table, &pin.name, entry)
        .context("Failed to insert transitive to workspace.dependencies")?;
    }

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(workspace_toml_path, &doc)?;

    Ok(())
  }

  /// Convert DepKind to Cargo.toml section name
  fn dep_kind_to_section(&self, dep_kind: DepKind) -> &'static str {
    manifest_ops::dep_kind_to_section(dep_kind)
  }

  /// Write MSRV (rust-version) to workspace manifest
  ///
  /// Writes to [workspace.package].rust-version so that members can inherit it
  /// via `rust-version.workspace = true`
  pub fn write_workspace_msrv(&self, workspace_toml_path: &Path, msrv: &semver::Version) -> RailResult<()> {
    // Read workspace Cargo.toml
    let mut doc = manifest_ops::read_toml_file(workspace_toml_path)?;

    // Ensure [workspace] section exists
    manifest_ops::ensure_section(&mut doc, "workspace").context("Failed to create [workspace] section")?;

    // Get or create [workspace.package] section
    let ws_package = manifest_ops::get_or_create_table(&mut doc, "workspace.package")
      .context("Failed to create [workspace.package]")?;

    // Format MSRV as "major.minor" (standard rust-version format)
    let msrv_str = format!("{}.{}", msrv.major, msrv.minor);

    // Insert or update rust-version
    ws_package.insert("rust-version", toml_edit::value(&msrv_str));

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(workspace_toml_path, &doc)?;

    Ok(())
  }

  /// Remove an unused dependency from a member's Cargo.toml
  ///
  /// # Arguments
  ///
  /// * `member_toml_path` - Path to the member's Cargo.toml
  /// * `dep_name` - Name of the dependency to remove
  /// * `dep_kind` - Type of dependency (Normal, Dev, Build)
  /// * `target` - Optional target platform constraint (e.g., "cfg(unix)")
  pub fn remove_dep(
    &self,
    member_toml_path: &Path,
    dep_name: &str,
    dep_kind: DepKind,
    target: Option<&str>,
  ) -> RailResult<()> {
    // Read member Cargo.toml
    let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

    // Get section name from kind
    let kind_section = self.dep_kind_to_section(dep_kind);

    // Handle target-specific vs regular sections
    if let Some(target_cfg) = target {
      // Target-specific: remove from [target.'cfg(...)'.dependencies]
      manifest_ops::remove_target_dependency(&mut doc, target_cfg, kind_section, dep_name)
        .context("Failed to remove target-specific dependency")?;
    } else {
      // Regular section: remove from [dependencies], [dev-dependencies], or [build-dependencies]
      if let Some(deps) = doc.get_mut(kind_section).and_then(|d| d.as_table_like_mut()) {
        deps.remove(dep_name);
      }
    }

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(member_toml_path, &doc)?;

    Ok(())
  }
}
