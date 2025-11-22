//! Issue detection for dependency unification

use super::path_handling::are_all_identical_workspace_paths;
use super::types::{DependencyInstance, IssueSeverity, IssueType, UnificationIssue};
use crate::cargo::WorkspaceMetadata;
use crate::error::RailResult;
use std::collections::{HashMap, HashSet};

/// Detect dependencies that exist in multiple resolved versions
///
/// Checks the actual dependency resolution graph to find packages with
/// multiple versions. These cannot be safely unified at workspace level.
pub fn detect_multi_version_conflicts(metadata: &WorkspaceMetadata) -> RailResult<Vec<UnificationIssue>> {
  let mut issues = Vec::new();

  let resolve = match metadata.resolve() {
    Some(r) => r,
    None => return Ok(issues), // No resolution available, skip check
  };

  // Group resolved packages by name
  let mut by_name: HashMap<String, Vec<&cargo_metadata::PackageId>> = HashMap::new();
  for node in &resolve.nodes {
    if let Some(pkg) = metadata.find_package_by_id(&node.id) {
      // Skip workspace members
      if metadata.get_package(&pkg.name).is_some() {
        continue;
      }

      by_name.entry(pkg.name.to_string()).or_default().push(&node.id);
    }
  }

  // Find packages with multiple resolved versions
  for (pkg_name, package_ids) in by_name {
    if package_ids.len() <= 1 {
      continue;
    }

    // Extract versions from package IDs
    let mut versions: Vec<String> = package_ids
      .iter()
      .filter_map(|id| metadata.find_package_by_id(id).map(|p| p.version.to_string()))
      .collect();

    // Deduplicate
    versions.sort();
    versions.dedup();

    if versions.len() > 1 {
      issues.push(UnificationIssue {
        dep_name: pkg_name.clone(),
        issue_type: IssueType::MultipleResolvedVersions {
          versions: versions.clone(),
        },
        severity: IssueSeverity::Soft, // Can be resolved via workspace.dependencies
        affected_members: vec![],
        suggestion: format!(
          "Multiple versions of '{}' found: {}. Standardize to a single version across the workspace.",
          pkg_name,
          versions.join(", ")
        ),
      });
    }
  }

  Ok(issues)
}

/// Detect issues preventing unification for a specific dependency
pub fn detect_issues(
  dep_name: &str,
  instances: &[DependencyInstance],
  allow_renamed: bool,
  metadata: &WorkspaceMetadata,
) -> Option<UnificationIssue> {
  // Check for renamed dependencies
  let renames: Vec<_> = instances
    .iter()
    .filter_map(|i| i.rename.as_ref().map(|r| (i.member.clone(), r.clone())))
    .collect();

  if !renames.is_empty() && !allow_renamed {
    return Some(UnificationIssue {
      dep_name: dep_name.to_string(),
      issue_type: IssueType::Renamed {
        original: dep_name.to_string(),
        renames,
      },
      severity: IssueSeverity::Hard, // Renames block unification
      affected_members: instances.iter().map(|i| i.member.clone()).collect(),
      suggestion: "Standardize dependency name across all workspace members".to_string(),
    });
  }

  // Check for version conflicts (after attempting merge in analyze())
  // If we reach here with different versions, they couldn't be merged
  let versions: std::collections::HashSet<_> = instances.iter().map(|i| &i.version_req).collect();
  if versions.len() > 1 {
    return Some(UnificationIssue {
      dep_name: dep_name.to_string(),
      issue_type: IssueType::IncompatibleVersionRequirements {
        requirements: instances
          .iter()
          .map(|i| (i.member.clone(), i.version_req.to_string()))
          .collect(),
      },
      severity: IssueSeverity::Soft, // Can be resolved via workspace.dependencies
      affected_members: instances.iter().map(|i| i.member.clone()).collect(),
      suggestion: "Standardize version requirement across all workspace members".to_string(),
    });
  }

  // Check for path dependencies
  let paths: Vec<_> = instances
    .iter()
    .filter_map(|i| i.path.as_ref().map(|p| (i.member.clone(), p.clone())))
    .collect();

  if !paths.is_empty() {
    // Allow workspace member path dependencies to be unified
    if are_all_identical_workspace_paths(&paths, metadata) {
      return None;
    }

    // Mixed or external paths - create issue
    return Some(UnificationIssue {
      dep_name: dep_name.to_string(),
      issue_type: IssueType::PathDependency { paths },
      severity: IssueSeverity::Hard, // Incompatible paths block unification
      affected_members: instances.iter().map(|i| i.member.clone()).collect(),
      suggestion:
        "Convert external path dependencies to version dependencies, or ensure all workspace members use the same path"
          .to_string(),
    });
  }

  // Check if all are target-specific
  if instances.iter().all(|i| i.target.is_some()) {
    let targets: Vec<_> = instances.iter().filter_map(|i| i.target.clone()).collect();

    // Check if all instances use the SAME target
    let unique_targets: HashSet<_> = targets.iter().collect();

    if unique_targets.len() == 1 {
      // All instances use the same target - we CAN unify this!
      // This is NOT an issue, it's a unification opportunity
      // Don't return an issue here - let the normal unification flow handle it
      // The unified dep can use [target.'cfg(...)'.dependencies]
      return None;
    }

    // Multiple different targets - this is more complex
    return Some(UnificationIssue {
      dep_name: dep_name.to_string(),
      issue_type: IssueType::AllTargetSpecific {
        targets: targets.clone(),
      },
      severity: IssueSeverity::Info, // Just informational
      affected_members: instances.iter().map(|i| i.member.clone()).collect(),
      suggestion: format!(
        "Multiple platform-specific targets detected: {}. Consider per-platform unification.",
        unique_targets
          .iter()
          .map(|t| format!("'{}'", t))
          .collect::<Vec<_>>()
          .join(", ")
      ),
    });
  }

  // Check for inconsistent default-features settings
  let with_default: Vec<_> = instances
    .iter()
    .filter(|i| i.default_features)
    .map(|i| i.member.clone())
    .collect();
  let without_default: Vec<_> = instances
    .iter()
    .filter(|i| !i.default_features)
    .map(|i| i.member.clone())
    .collect();

  if !with_default.is_empty() && !without_default.is_empty() {
    return Some(UnificationIssue {
      dep_name: dep_name.to_string(),
      issue_type: IssueType::InconsistentDefaultFeatures {
        members_with_default: with_default.clone(),
        members_without_default: without_default.clone(),
      },
      severity: IssueSeverity::Soft, // Can be resolved via workspace.dependencies
      affected_members: instances.iter().map(|i| i.member.clone()).collect(),
      suggestion: "Standardize default-features setting. Consider setting default-features = false and explicitly enabling needed features".to_string(),
    });
  }

  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use cargo_metadata::{DependencyKind, camino::Utf8PathBuf};
  use semver::VersionReq;

  fn create_test_metadata() -> WorkspaceMetadata {
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceMetadata::load(&current_dir).unwrap()
  }

  fn create_instance(member: &str, version: &str) -> DependencyInstance {
    DependencyInstance {
      member: member.to_string(),
      name: "test-dep".to_string(),
      version_req: VersionReq::parse(version).unwrap(),
      features: vec![],
      feature_provenance: std::collections::HashMap::new(),
      default_features: true,
      kind: DependencyKind::Normal,
      target: None,
      rename: None,
      path: None,
      is_proc_macro: false,
    }
  }

  #[test]
  fn test_detect_renamed_dependency_hard() {
    let metadata = create_test_metadata();
    let mut instance1 = create_instance("member1", "1.0.0");
    instance1.rename = Some("renamed-dep".to_string());

    let instance2 = create_instance("member2", "1.0.0");

    let instances = vec![instance1, instance2];

    let issue = detect_issues("test-dep", &instances, false, &metadata).unwrap();

    assert_eq!(issue.severity, IssueSeverity::Hard);
    if let IssueType::Renamed { .. } = issue.issue_type {
      // OK
    } else {
      panic!("Expected Renamed issue type");
    }
  }

  #[test]
  fn test_detect_version_conflict_soft() {
    let metadata = create_test_metadata();
    let instance1 = create_instance("member1", "1.0.0");
    let instance2 = create_instance("member2", "2.0.0");

    let instances = vec![instance1, instance2];

    let issue = detect_issues("test-dep", &instances, false, &metadata).unwrap();

    assert_eq!(issue.severity, IssueSeverity::Soft);
    if let IssueType::IncompatibleVersionRequirements { .. } = issue.issue_type {
      // OK
    } else {
      panic!("Expected IncompatibleVersionRequirements issue type");
    }
  }

  #[test]
  fn test_detect_inconsistent_default_features_soft() {
    let metadata = create_test_metadata();
    let mut instance1 = create_instance("member1", "1.0.0");
    instance1.default_features = true;

    let mut instance2 = create_instance("member2", "1.0.0");
    instance2.default_features = false;

    let instances = vec![instance1, instance2];

    let issue = detect_issues("test-dep", &instances, false, &metadata).unwrap();

    assert_eq!(issue.severity, IssueSeverity::Soft);
    if let IssueType::InconsistentDefaultFeatures { .. } = issue.issue_type {
      // OK
    } else {
      panic!("Expected InconsistentDefaultFeatures issue type");
    }
  }

  #[test]
  fn test_detect_path_dependency_hard() {
    let metadata = create_test_metadata();
    let mut instance1 = create_instance("member1", "1.0.0");
    instance1.path = Some(Utf8PathBuf::from("/some/external/path"));

    let instance2 = create_instance("member2", "1.0.0");

    let instances = vec![instance1, instance2];

    let issue = detect_issues("test-dep", &instances, false, &metadata).unwrap();

    assert_eq!(issue.severity, IssueSeverity::Hard);
    if let IssueType::PathDependency { .. } = issue.issue_type {
      // OK
    } else {
      panic!("Expected PathDependency issue type");
    }
  }

  #[test]
  fn test_detect_target_specific_info() {
    let metadata = create_test_metadata();
    let mut instance1 = create_instance("member1", "1.0.0");
    instance1.target = Some("cfg(unix)".to_string());

    let mut instance2 = create_instance("member2", "1.0.0");
    instance2.target = Some("cfg(windows)".to_string());

    let instances = vec![instance1, instance2];

    let issue = detect_issues("test-dep", &instances, false, &metadata).unwrap();

    assert_eq!(issue.severity, IssueSeverity::Info);
    if let IssueType::AllTargetSpecific { .. } = issue.issue_type {
      // OK
    } else {
      panic!("Expected AllTargetSpecific issue type");
    }
  }
}
