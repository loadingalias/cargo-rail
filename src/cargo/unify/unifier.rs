//! Core unification logic

use super::path_handling::normalize_workspace_path;
use super::types::{DependencyInstance, UnifiedDep};
use crate::cargo::WorkspaceMetadata;
use crate::error::RailResult;
use std::collections::{HashMap, HashSet};

/// Unify dependency instances into a single workspace dependency
pub fn unify_instances(
  dep_name: &str,
  instances: &[DependencyInstance],
  metadata: &WorkspaceMetadata,
) -> RailResult<UnifiedDep> {
  // Version: use first (already validated all are identical or successfully merged)
  let version_req = instances[0].version_req.clone();

  // Features: union of all
  let mut all_features = HashSet::new();
  for instance in instances {
    all_features.extend(instance.features.iter().cloned());
  }

  if dep_name == "reqwest" {
    println!("\n==== REQWEST DEBUG ====");
    println!("all_features from instances: {:?}", all_features);
  }

  // Get package info to determine which features are explicitly defined
  let pkg_info = metadata
    .metadata_json()
    .packages
    .iter()
    .find(|pkg| pkg.name == dep_name);

  let (explicit_features, optional_deps): (HashSet<String>, HashSet<String>) = if let Some(pkg) = pkg_info {
    let explicit = pkg.features.keys().cloned().collect();
    let optional = pkg
      .dependencies
      .iter()
      .filter(|dep| dep.optional)
      .map(|dep| dep.name.clone())
      .collect();
    (explicit, optional)
  } else {
    (HashSet::new(), HashSet::new())
  };

  if dep_name == "reqwest" {
    println!("explicit_features: {:?}", explicit_features);
    println!("optional_deps: {:?}", optional_deps);
  }

  // Filter out:
  // 1. Internal/private features (starting with __)
  // 2. Names that are ONLY optional dependencies (not explicitly defined as features)
  //    Example: "hyper-rustls" is an optional dep without a matching feature
  //    Counter-example: "tokio" is both an optional dep AND an explicit feature (keep it)
  let mut features: Vec<_> = all_features
    .into_iter()
    .filter(|f| {
      if f.starts_with("__") {
        return false; // Filter out internal features
      }
      // If it's an optional dependency name, only keep it if it's also an explicit feature
      if optional_deps.contains(f) {
        return explicit_features.contains(f);
      }
      true // Keep all other features
    })
    .collect();
  features.sort();

  // Feature provenance: collect ALL sources for each feature across instances
  let mut feature_provenance: HashMap<String, Vec<super::types::FeatureSource>> = HashMap::new();
  for instance in instances {
    for (feature, source) in &instance.feature_provenance {
      feature_provenance
        .entry(feature.clone())
        .or_default()
        .push(source.clone());
    }
  }

  // Default features: true if ANY instance uses them
  let default_features = instances.iter().any(|i| i.default_features);

  // Used by
  let mut used_by: Vec<_> = instances.iter().map(|i| i.member.clone()).collect();
  used_by.sort();
  used_by.dedup();

  // Dependency kinds
  let dep_kinds: HashSet<_> = instances.iter().map(|i| i.kind).collect();

  // Fragmentation count (unique feature combinations)
  let unique_feature_sets: HashSet<_> = instances
    .iter()
    .map(|i| {
      let mut f = i.features.clone();
      f.sort();
      f
    })
    .collect();
  let fragmentation_count = unique_feature_sets.len();

  // Path: if all instances have the same workspace member path, preserve it
  let path = if instances.iter().all(|i| i.path.is_some()) {
    let paths: Vec<_> = instances.iter().filter_map(|i| i.path.as_ref()).collect();
    if !paths.is_empty() {
      let first = paths[0];
      if paths
        .iter()
        .all(|p| normalize_workspace_path(p, metadata) == normalize_workspace_path(first, metadata))
      {
        // All paths are identical and point to workspace member
        Some(normalize_workspace_path(first, metadata))
      } else {
        None
      }
    } else {
      None
    }
  } else {
    None
  };

  // Target: if all instances have the SAME target, preserve it for workspace-level unification
  let target = if instances.iter().all(|i| i.target.is_some()) {
    let targets: Vec<_> = instances.iter().filter_map(|i| i.target.as_ref()).collect();
    if !targets.is_empty() {
      let first = targets[0];
      if targets.iter().all(|t| t == &first) {
        // All targets are identical - we can unify with target specifier!
        Some(first.clone())
      } else {
        None
      }
    } else {
      None
    }
  } else {
    None
  };

  // Proc-macro detection: if ANY instance is a proc-macro, mark the unified dep
  // (all instances SHOULD have the same value since they're the same package)
  let is_proc_macro = instances.iter().any(|i| i.is_proc_macro);

  // Generate standard comment showing which members use this dep
  let member_count = used_by.len();
  let comment = if fragmentation_count > 1 {
    format!(
      "Unified from {} members ({} feature combinations)",
      member_count, fragmentation_count
    )
  } else {
    format!(
      "Unified from {} member{}",
      member_count,
      if member_count == 1 { "" } else { "s" }
    )
  };

  Ok(UnifiedDep {
    name: dep_name.to_string(),
    version_req,
    features,
    feature_provenance,
    default_features,
    used_by,
    dep_kinds,
    fragmentation_count,
    path,
    target,
    comments: vec![comment],
    is_proc_macro,
  })
}
