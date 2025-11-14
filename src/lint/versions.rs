//! Version linting: detect duplicate dependency versions

use crate::core::config::PolicyConfig;
use crate::core::error::{RailResult, ResultExt};
use cargo_metadata::{Metadata, Package, PackageId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

/// A single version conflict found during linting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionIssue {
  /// The dependency name with multiple versions
  pub dependency_name: String,
  /// All versions found in the workspace
  pub versions: Vec<String>,
  /// Crates using each version
  pub usage_by_version: HashMap<String, Vec<String>>,
  /// Suggested unified version (highest semver-compatible)
  pub suggested_version: String,
  /// Whether this dependency is in forbid_multiple_versions list
  pub is_forbidden: bool,
}

/// Report of all version conflicts found
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionsReport {
  /// Total number of dependencies with multiple versions
  pub total_conflicts: usize,
  /// Issues found
  pub issues: Vec<VersionIssue>,
  /// Count of forbidden conflicts (from policy)
  pub forbidden_count: usize,
}

/// Version linter for duplicate dependency detection
pub struct VersionsLinter {
  metadata: Metadata,
  policy: Option<PolicyConfig>,
}

impl VersionsLinter {
  /// Create a new linter from cargo metadata and optional policy
  pub fn new(metadata: Metadata, policy: Option<PolicyConfig>) -> Self {
    Self { metadata, policy }
  }

  /// Analyze workspace and find version conflicts
  pub fn analyze(&self) -> RailResult<VersionsReport> {
    // Build dependency version map: dep_name -> [(version, [using_crates])]
    let mut dep_versions: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();

    // Collect all dependencies from workspace packages
    for package in self.metadata.workspace_packages() {
      self.collect_dependencies(package, &mut dep_versions);
    }

    // Find conflicts (dependencies with multiple versions)
    let mut issues = Vec::new();
    let forbidden_set: HashSet<&str> = self
      .policy
      .as_ref()
      .map(|p| p.forbid_multiple_versions.iter().map(|s| s.as_str()).collect())
      .unwrap_or_default();

    for (dep_name, versions) in dep_versions {
      if versions.len() > 1 {
        // Multiple versions found - create issue
        let is_forbidden = forbidden_set.contains(dep_name.as_str());

        // Convert HashSet to Vec for serialization
        let usage_by_version: HashMap<String, Vec<String>> = versions
          .iter()
          .map(|(v, crates)| (v.clone(), crates.iter().cloned().collect()))
          .collect();

        // Suggest highest version (simple heuristic)
        let suggested_version = self.suggest_unified_version(&versions);

        issues.push(VersionIssue {
          dependency_name: dep_name,
          versions: versions.keys().cloned().collect(),
          usage_by_version,
          suggested_version,
          is_forbidden,
        });
      }
    }

    // Sort issues: forbidden first, then alphabetically
    issues.sort_by(|a, b| match (a.is_forbidden, b.is_forbidden) {
      (true, false) => std::cmp::Ordering::Less,
      (false, true) => std::cmp::Ordering::Greater,
      _ => a.dependency_name.cmp(&b.dependency_name),
    });

    let forbidden_count = issues.iter().filter(|i| i.is_forbidden).count();

    Ok(VersionsReport {
      total_conflicts: issues.len(),
      issues,
      forbidden_count,
    })
  }

  /// Collect all dependencies from a package
  fn collect_dependencies(
    &self,
    package: &Package,
    dep_versions: &mut HashMap<String, HashMap<String, HashSet<String>>>,
  ) {
    for dep in &package.dependencies {
      // Resolve the actual package this dependency points to
      if let Some(resolved) = self.resolve_dependency(&dep.name, &dep.req) {
        let versions = dep_versions.entry(dep.name.clone()).or_default();
        let crates = versions.entry(resolved.version.clone()).or_default();
        crates.insert(package.name.to_string());
      }
    }
  }

  /// Resolve dependency to actual package version
  /// This uses cargo's resolution from metadata
  fn resolve_dependency(&self, name: &str, _req: &semver::VersionReq) -> Option<VersionInfo> {
    // Find all packages with this name
    let packages: Vec<_> = self.metadata.packages.iter().filter(|p| p.name == name).collect();

    // For now, collect all versions found
    // In practice, cargo metadata already shows resolved versions
    packages.first().map(|p| VersionInfo {
      version: p.version.to_string(),
      package_id: p.id.clone(),
    })
  }

  /// Suggest unified version (simple heuristic: choose highest)
  fn suggest_unified_version(&self, versions: &HashMap<String, HashSet<String>>) -> String {
    let mut version_list: Vec<_> = versions.keys().collect();
    version_list.sort_by(|a, b| {
      // Try to parse as semver, fall back to string comparison
      match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(va), Ok(vb)) => vb.cmp(&va), // Highest first
        _ => b.cmp(a),                   // String comparison
      }
    });

    version_list.first().map(|s| s.to_string()).unwrap_or_default()
  }

  /// Apply fixes to unify versions (requires --apply)
  pub fn fix(&self, report: &VersionsReport, apply: bool) -> RailResult<FixReport> {
    let mut fixed = Vec::new();

    for issue in &report.issues {
      // For each crate using non-suggested versions, update it
      for (version, crates) in &issue.usage_by_version {
        if version != &issue.suggested_version {
          for crate_name in crates {
            // Find the package
            if let Some(package) = self
              .metadata
              .workspace_packages()
              .iter()
              .find(|p| p.name == *crate_name)
            {
              let manifest_path = package.manifest_path.as_std_path();

              // Try to fix this crate's manifest
              if let Ok(fix) = self.fix_manifest(
                manifest_path,
                &issue.dependency_name,
                version,
                &issue.suggested_version,
                apply,
              ) && let Some(f) = fix
              {
                fixed.push(f);
              }
            }
          }
        }
      }
    }

    Ok(FixReport {
      total_fixed: fixed.len(),
      fixed,
      dry_run: !apply,
    })
  }

  /// Fix version in a single manifest
  fn fix_manifest(
    &self,
    path: &Path,
    dep_name: &str,
    current_version: &str,
    new_version: &str,
    apply: bool,
  ) -> RailResult<Option<FixedVersion>> {
    let content = std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    let mut doc = content
      .parse::<DocumentMut>()
      .with_context(|| format!("Failed to parse {}", path.display()))?;

    let mut fixed = false;
    let mut section_name = String::new();

    // Check all dependency sections
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
      if let Some(deps) = doc.get_mut(section).and_then(|d| d.as_table_mut())
        && let Some(dep) = deps.get_mut(dep_name)
      {
        // Update version string
        if Self::update_version_in_item(dep, current_version, new_version) {
          fixed = true;
          section_name = section.to_string();
          break;
        }
      }
    }

    if !fixed {
      return Ok(None);
    }

    // Write back if apply=true
    if apply {
      std::fs::write(path, doc.to_string()).with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(Some(FixedVersion {
      manifest_path: path.to_path_buf(),
      dependency_name: dep_name.to_string(),
      section: section_name,
      from_version: current_version.to_string(),
      to_version: new_version.to_string(),
    }))
  }

  /// Update version in TOML item (handles both string and table format)
  fn update_version_in_item(item: &mut Item, _current: &str, new_version: &str) -> bool {
    // Handle string format: dep = "1.0"
    if item.as_str().is_some() {
      *item = Item::Value(toml_edit::Value::String(toml_edit::Formatted::new(
        new_version.to_string(),
      )));
      return true;
    }

    // Handle table format: dep = { version = "1.0", features = [...] }
    if let Some(table) = item.as_inline_table_mut()
      && let Some(version) = table.get_mut("version")
    {
      *version = toml_edit::Value::String(toml_edit::Formatted::new(new_version.to_string()));
      return true;
    }

    if let Some(table) = item.as_table_mut()
      && let Some(version) = table.get_mut("version")
    {
      *version = Item::Value(toml_edit::Value::String(toml_edit::Formatted::new(
        new_version.to_string(),
      )));
      return true;
    }

    false
  }
}

#[derive(Debug, Clone)]
struct VersionInfo {
  version: String,
  #[allow(dead_code)] // TODO(Pillar 3): Use for more sophisticated version resolution
  package_id: PackageId,
}

/// Report of fixes applied
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixReport {
  pub total_fixed: usize,
  pub fixed: Vec<FixedVersion>,
  pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedVersion {
  pub manifest_path: PathBuf,
  pub dependency_name: String,
  pub section: String,
  pub from_version: String,
  pub to_version: String,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_version_suggestion() {
    let mut versions = HashMap::new();
    versions.insert("1.0.0".to_string(), HashSet::new());
    versions.insert("1.1.0".to_string(), HashSet::new());
    versions.insert("0.9.0".to_string(), HashSet::new());

    let linter = VersionsLinter::new(cargo_metadata::MetadataCommand::new().exec().unwrap(), None);

    let suggested = linter.suggest_unified_version(&versions);
    // Should suggest highest version
    assert_eq!(suggested, "1.1.0");
  }

  #[test]
  fn test_version_item_update() {
    // Test string format
    let mut item = toml_edit::value("1.0.0");
    assert!(VersionsLinter::update_version_in_item(&mut item, "1.0.0", "2.0.0"));
    assert_eq!(item.as_str(), Some("2.0.0"));
  }
}
