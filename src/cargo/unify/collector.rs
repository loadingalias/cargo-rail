//! Dependency collection from workspace members

use super::path_handling::is_workspace_member_path;
use super::types::DependencyInstance;
use crate::cargo::WorkspaceMetadata;
use std::collections::{HashMap, HashSet};

/// Collect all dependency instances from workspace members
///
/// Uses RESOLVED features from the dependency graph, not declared features.
/// This ensures we capture features enabled transitively and provides the
/// TRUE feature union across the workspace.
pub fn collect_dependencies(metadata: &WorkspaceMetadata) -> Vec<DependencyInstance> {
  let mut instances = Vec::new();

  for pkg in metadata.list_crates() {
    // Check which dependencies in this member use workspace = true
    let workspace_inherited = get_member_workspace_deps(&pkg.manifest_path);

    for dep in &pkg.dependencies {
      // Skip dependencies that are already using workspace inheritance in THIS member
      if workspace_inherited.contains(&dep.name) {
        continue;
      }
      // Get RESOLVED features if available, fallback to declared features
      // Resolved features include:
      // 1. Features declared in this member's Cargo.toml
      // 2. Default features (if enabled)
      // 3. Features activated by other workspace members
      // 4. Features activated transitively through dependency chains
      let features = if let Some(resolved_features) = metadata.get_resolved_features_for_package(&dep.name) {
        // Use resolved features - this is the ACTUAL set Cargo enables
        resolved_features.into_iter().collect()
      } else {
        // Fallback to declared features if:
        // - No resolve graph available
        // - Package is a workspace member
        // - Package not found in resolved graph (e.g., optional dep not enabled)
        dep.features.clone()
      };

      instances.push(DependencyInstance {
        member: pkg.name.to_string(),
        name: dep.name.clone(),
        version_req: dep.req.clone(),
        features,
        default_features: dep.uses_default_features,
        optional: dep.optional,
        kind: dep.kind,
        target: dep.target.as_ref().map(|t| t.to_string()),
        rename: dep.rename.clone(),
        path: dep.path.clone(),
      });
    }
  }

  instances
}

/// Group dependency instances by package name
pub fn group_by_name(
  instances: Vec<DependencyInstance>,
  metadata: &WorkspaceMetadata,
) -> HashMap<String, Vec<DependencyInstance>> {
  let mut grouped: HashMap<String, Vec<DependencyInstance>> = HashMap::new();

  for instance in instances {
    // Use the actual dependency name
    let dep_name = instance.name.clone();

    // Skip workspace member path dependencies
    if let Some(ref path) = instance.path
      && is_workspace_member_path(path, metadata)
    {
      continue;
    }

    grouped.entry(dep_name).or_default().push(instance);
  }

  grouped
}

/// Get dependencies that use workspace = true in a member's Cargo.toml
fn get_member_workspace_deps(manifest_path: &cargo_metadata::camino::Utf8Path) -> HashSet<String> {
  let mut workspace_deps = HashSet::new();

  if let Ok(content) = std::fs::read_to_string(manifest_path.as_std_path())
    && let Ok(doc) = content.parse::<toml_edit::DocumentMut>()
  {
    // Check all dependency sections
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
      if let Some(deps) = doc.get(section).and_then(|d| d.as_table()) {
        for (key, value) in deps.iter() {
          // Check if this dep uses workspace = true
          let uses_workspace = if let Some(inline_table) = value.as_inline_table() {
            inline_table.get("workspace").and_then(|w| w.as_bool()) == Some(true)
          } else if let Some(table) = value.as_table() {
            table.get("workspace").and_then(|w| w.as_bool()) == Some(true)
          } else {
            false
          };

          if uses_workspace {
            workspace_deps.insert(key.to_string());
          }
        }
      }
    }
  }

  workspace_deps
}
