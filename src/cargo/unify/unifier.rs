//! Core unification logic

use super::path_handling::normalize_workspace_path;
use super::types::{DependencyInstance, UnifiedDep};
use crate::cargo::WorkspaceMetadata;
use crate::error::RailResult;
use std::collections::HashSet;

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
  let mut features: Vec<_> = all_features.into_iter().collect();
  features.sort();

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

  Ok(UnifiedDep {
    name: dep_name.to_string(),
    version_req,
    features,
    default_features,
    used_by,
    dep_kinds,
    fragmentation_count,
    path,
  })
}
