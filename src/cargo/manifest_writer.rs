//! Clean manifest writing operations
//!
//! Handles writing [workspace.dependencies] and updating member manifests

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::manifest_ops;
use crate::cargo::unify_analyzer::UnifiedDep;
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
  pub fn add_transitive_pins(
    &self,
    host_toml_path: &Path,
    transitives: &[(String, Vec<String>)], // (dep_name, features)
  ) -> RailResult<()> {
    // Read host Cargo.toml (usually workspace root)
    let mut doc = manifest_ops::read_toml_file(host_toml_path)?;

    // Ensure [dev-dependencies] exists
    let dev_deps =
      manifest_ops::get_or_create_table(&mut doc, "dev-dependencies").context("Failed to create [dev-dependencies]")?;

    // Add each transitive as a dev dependency
    for (dep_name, features) in transitives {
      let entry = manifest_ops::build_transitive_entry(features);
      manifest_ops::insert_dependency(dev_deps, dep_name, entry).context("Failed to insert transitive dependency")?;
    }

    // Format and write
    self.formatter.format_manifest(&mut doc)?;
    manifest_ops::write_toml_file(host_toml_path, &doc)?;

    Ok(())
  }

  /// Convert DepKind to Cargo.toml section name
  fn dep_kind_to_section(&self, dep_kind: DepKind) -> &'static str {
    manifest_ops::dep_kind_to_section(dep_kind)
  }
}
