//! Dependency collection from workspace members

use super::path_handling::is_workspace_member_path;
use super::types::{DependencyInstance, FeatureSource};
use crate::cargo::WorkspaceMetadata;
use std::collections::HashMap;

/// Collect all dependency instances from workspace members
///
/// Uses RESOLVED features from the dependency graph, not declared features.
/// This ensures we capture features enabled transitively and provides the
/// TRUE feature union across the workspace.
///
/// The `use_all_features` parameter indicates whether metadata was collected with --all-features,
/// which affects how we classify unidentified feature sources.
pub fn collect_dependencies(metadata: &WorkspaceMetadata, use_all_features: bool) -> Vec<DependencyInstance> {
  let mut instances = Vec::new();

  for pkg in metadata.list_crates() {
    for dep in &pkg.dependencies {
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
      let feature_provenance = determine_feature_provenance(&features, dep, &pkg.name, metadata, use_all_features);

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

/// Determine WHY each feature is enabled for a dependency
///
/// This is the key to showing users Cargo.toml-level information about their dependencies.
/// We check multiple sources in order of specificity:
/// 1. Direct declaration in member's Cargo.toml
/// 2. Default features
/// 3. Target-specific
/// 4. Transitive or --all-features (depending on use_all_features parameter)
fn determine_feature_provenance(
  resolved_features: &[String],
  dep: &cargo_metadata::Dependency,
  member_name: &str,
  metadata: &WorkspaceMetadata,
  use_all_features: bool,
) -> HashMap<String, FeatureSource> {
  let mut provenance = HashMap::new();

  // Get the actual package metadata for this dependency
  let dep_pkg = metadata.get_package(&dep.name);

  // Build a set of "root" features that could transitively enable other features:
  // 1. Features explicitly declared in Cargo.toml
  // 2. Default features (if enabled)
  let mut root_features = dep.features.to_vec();
  if dep.uses_default_features {
    root_features.push("default".to_string());
  }

  for feature in resolved_features {
    let source = determine_single_feature_source(feature, dep, member_name, dep_pkg, &root_features, use_all_features);
    provenance.insert(feature.clone(), source);
  }

  provenance
}

/// Determine the source of a single feature
///
/// `root_features` are the features that were explicitly enabled (from Cargo.toml + default if enabled)
fn determine_single_feature_source(
  feature: &str,
  dep: &cargo_metadata::Dependency,
  member_name: &str,
  dep_pkg: Option<&cargo_metadata::Package>,
  root_features: &[String],
  use_all_features: bool,
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
    // Default features can reference other features directly
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

  // 4. Check if it's enabled transitively through a feature dependency chain
  // We need to trace from root_features (explicitly enabled) to this feature
  if let Some(pkg) = dep_pkg {
    // Try to find a path from any root feature to this feature
    for root_feature in root_features {
      if let Some(chain) = find_feature_chain(root_feature, feature, pkg) {
        return FeatureSource::Transitive { through: chain };
      }
    }
  }

  // 5. If metadata was collected with --all-features, classify as such
  // Otherwise, it's transitive but we couldn't trace the exact chain
  if use_all_features {
    FeatureSource::AllFeatures
  } else {
    // Transitive dependency - exact chain couldn't be determined
    FeatureSource::Transitive { through: vec![] }
  }
}

/// Find a chain of features from `start` to `target`
///
/// Returns the feature chain if found (e.g., ["perf", "perf-inline"])
fn find_feature_chain(start: &str, target: &str, pkg: &cargo_metadata::Package) -> Option<Vec<String>> {
  // Check if start directly enables target
  if let Some(feature_deps) = pkg.features.get(start) {
    for dep_feature in feature_deps {
      let clean_name = dep_feature.trim_start_matches("dep:");
      if clean_name == target {
        return Some(vec![start.to_string()]);
      }
    }

    // Check if start enables something that enables target (one level deep)
    for dep_feature in feature_deps {
      let intermediate = dep_feature.trim_start_matches("dep:");
      if let Some(intermediate_deps) = pkg.features.get(intermediate) {
        for sub_dep in intermediate_deps {
          if sub_dep.trim_start_matches("dep:") == target {
            return Some(vec![start.to_string(), intermediate.to_string()]);
          }
        }
      }
    }
  }

  None
}
