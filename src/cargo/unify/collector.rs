//! Dependency collection from workspace members

use super::path_handling::is_workspace_member_path;
use super::types::{DependencyInstance, FeatureSource};
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

      // Track feature provenance: WHY is each feature enabled?
      let feature_provenance = determine_feature_provenance(&features, dep, &pkg.name, metadata);

      // Detect if this is a proc-macro crate
      // Proc-macros are build-time only and have different optimization strategies
      let is_proc_macro = metadata.is_proc_macro_crate(&dep.name);

      instances.push(DependencyInstance {
        member: pkg.name.to_string(),
        name: dep.name.clone(),
        version_req: dep.req.clone(),
        features,
        feature_provenance,
        default_features: dep.uses_default_features,
        kind: dep.kind,
        target: dep.target.as_ref().map(|t| t.to_string()),
        rename: dep.rename.clone(),
        path: dep.path.clone(),
        is_proc_macro,
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

/// Determine WHY each feature is enabled for a dependency
///
/// This is the key to showing users Cargo.toml-level information about their dependencies.
/// We check multiple sources in order of specificity:
/// 1. Direct declaration in member's Cargo.toml
/// 2. Default features
/// 3. Target-specific
/// 4. Transitive or --all-features
fn determine_feature_provenance(
  resolved_features: &[String],
  dep: &cargo_metadata::Dependency,
  member_name: &str,
  metadata: &WorkspaceMetadata,
) -> HashMap<String, FeatureSource> {
  let mut provenance = HashMap::new();

  // Get the actual package metadata for this dependency
  let dep_pkg = metadata.get_package(&dep.name);

  for feature in resolved_features {
    let source = determine_single_feature_source(feature, dep, member_name, dep_pkg);
    provenance.insert(feature.clone(), source);
  }

  provenance
}

/// Determine the source of a single feature
fn determine_single_feature_source(
  feature: &str,
  dep: &cargo_metadata::Dependency,
  member_name: &str,
  dep_pkg: Option<&cargo_metadata::Package>,
) -> FeatureSource {
  // 1. Check if declared directly in this member's Cargo.toml
  if dep.features.contains(&feature.to_string()) {
    return FeatureSource::Direct {
      member: member_name.to_string(),
    };
  }

  // 2. Check if it's a default feature (and default features are enabled)
  if dep.uses_default_features
    && let Some(pkg) = dep_pkg
    && let Some(default_features) = pkg.features.get("default")
  {
    // Default features can reference other features
    if default_features.iter().any(|f| f.trim_start_matches("dep:") == feature) {
      return FeatureSource::Default;
    }
  }

  // 3. Check if target-specific
  if let Some(ref target) = dep.target {
    return FeatureSource::TargetSpecific {
      target: target.to_string(),
    };
  }

  // 4. Otherwise it's either transitive or from --all-features
  // For now, we classify it as AllFeatures since we're using --all-features metadata
  // In a future enhancement, we could trace the actual dependency chain
  FeatureSource::AllFeatures
}
