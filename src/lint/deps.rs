//! Dependency linting: detect workspace dependencies that should use inheritance

use crate::core::error::{RailResult, ResultExt};
use cargo_metadata::{Metadata, Package};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Value};

/// A single dependency issue found during linting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepsIssue {
  /// The crate that has the issue
  pub crate_name: String,
  /// Path to the crate's Cargo.toml
  pub manifest_path: PathBuf,
  /// The dependency that should use workspace inheritance
  pub dependency_name: String,
  /// Which section: dependencies, dev-dependencies, or build-dependencies
  pub section: String,
  /// Current version/spec in the crate's Cargo.toml
  pub current_spec: String,
  /// Suggested fix (use workspace.dependencies)
  pub suggested_fix: String,
}

/// Report of all dependency issues found
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepsReport {
  /// Total number of issues found
  pub total_issues: usize,
  /// Issues grouped by crate
  pub issues: Vec<DepsIssue>,
  /// Summary: which dependencies are affected
  pub affected_dependencies: HashSet<String>,
}

/// Dependency linter for workspace inheritance detection
pub struct DepsLinter {
  metadata: Metadata,
}

impl DepsLinter {
  /// Create a new linter from cargo metadata
  pub fn new(metadata: Metadata) -> Self {
    Self { metadata }
  }

  /// Analyze workspace and find dependency issues
  pub fn analyze(&self) -> RailResult<DepsReport> {
    let workspace_deps = self.get_workspace_dependencies()?;
    let mut issues = Vec::new();
    let mut affected = HashSet::new();

    // Only check workspace members (not dependencies)
    for package in self
      .metadata
      .workspace_packages()
      .iter()
      .filter(|p| p.manifest_path.starts_with(&self.metadata.workspace_root))
    {
      issues.extend(self.check_package(package, &workspace_deps, &mut affected)?);
    }

    Ok(DepsReport {
      total_issues: issues.len(),
      issues,
      affected_dependencies: affected,
    })
  }

  /// Get workspace-level dependencies from root Cargo.toml
  fn get_workspace_dependencies(&self) -> RailResult<HashMap<String, WorkspaceDep>> {
    let workspace_toml = self.metadata.workspace_root.join("Cargo.toml");
    let content =
      std::fs::read_to_string(&workspace_toml).with_context(|| format!("Failed to read {}", workspace_toml))?;

    let doc = content
      .parse::<DocumentMut>()
      .with_context(|| format!("Failed to parse {}", workspace_toml))?;

    let mut workspace_deps = HashMap::new();

    // Extract [workspace.dependencies]
    if let Some(workspace) = doc.get("workspace").and_then(|w| w.as_table())
      && let Some(deps) = workspace.get("dependencies").and_then(|d| d.as_table())
    {
      for (name, spec) in deps.iter() {
        workspace_deps.insert(
          name.to_string(),
          WorkspaceDep {
            name: name.to_string(),
            spec: spec.to_string(),
          },
        );
      }
    }

    Ok(workspace_deps)
  }

  /// Check a single package for dependency issues
  fn check_package(
    &self,
    package: &Package,
    workspace_deps: &HashMap<String, WorkspaceDep>,
    affected: &mut HashSet<String>,
  ) -> RailResult<Vec<DepsIssue>> {
    let mut issues = Vec::new();

    // Read the package's Cargo.toml
    let manifest_path = &package.manifest_path;
    let content = std::fs::read_to_string(manifest_path.as_std_path())
      .with_context(|| format!("Failed to read {}", manifest_path))?;

    let doc = content
      .parse::<DocumentMut>()
      .with_context(|| format!("Failed to parse {}", manifest_path))?;

    // Check each dependency section
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
      if let Some(deps) = doc.get(section).and_then(|d| d.as_table()) {
        for (dep_name, dep_spec) in deps.iter() {
          // Skip if already using workspace inheritance
          if let Some(table) = dep_spec.as_inline_table() {
            if table.contains_key("workspace") {
              continue;
            }
          } else if let Some(table) = dep_spec.as_table()
            && table.contains_key("workspace")
          {
            continue;
          }

          // Check if this dependency is defined in workspace.dependencies
          if workspace_deps.contains_key(dep_name) {
            affected.insert(dep_name.to_string());
            issues.push(DepsIssue {
              crate_name: package.name.to_string(),
              manifest_path: manifest_path.clone().into_std_path_buf(),
              dependency_name: dep_name.to_string(),
              section: section.to_string(),
              current_spec: dep_spec.to_string(),
              suggested_fix: "{ workspace = true }".to_string(),
            });
          }
        }
      }
    }

    Ok(issues)
  }

  /// Apply fixes to all issues (requires --apply)
  pub fn fix(&self, report: &DepsReport, apply: bool) -> RailResult<FixReport> {
    let mut fixed = Vec::new();

    // Group issues by manifest path
    let mut by_manifest: HashMap<PathBuf, Vec<&DepsIssue>> = HashMap::new();
    for issue in &report.issues {
      by_manifest.entry(issue.manifest_path.clone()).or_default().push(issue);
    }

    for (manifest_path, issues) in by_manifest {
      let fixes = self.fix_manifest(&manifest_path, &issues, apply)?;
      fixed.extend(fixes);
    }

    Ok(FixReport {
      total_fixed: fixed.len(),
      fixed,
      dry_run: !apply,
    })
  }

  /// Fix a single manifest file
  fn fix_manifest(&self, path: &Path, issues: &[&DepsIssue], apply: bool) -> RailResult<Vec<FixedIssue>> {
    let content = std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    let mut doc = content
      .parse::<DocumentMut>()
      .with_context(|| format!("Failed to parse {}", path.display()))?;

    let mut fixed = Vec::new();

    for issue in issues {
      // Update the dependency to use workspace inheritance
      if let Some(section) = doc.get_mut(&issue.section).and_then(|s| s.as_table_mut())
        && let Some(dep) = section.get_mut(&issue.dependency_name)
      {
        // Create inline table: { workspace = true }
        let mut workspace_dep = toml_edit::InlineTable::new();
        workspace_dep.insert("workspace", Value::Boolean(toml_edit::Formatted::new(true)));

        *dep = Item::Value(Value::InlineTable(workspace_dep));

        fixed.push(FixedIssue {
          crate_name: issue.crate_name.clone(),
          dependency_name: issue.dependency_name.clone(),
          section: issue.section.clone(),
          before: issue.current_spec.clone(),
          after: "{ workspace = true }".to_string(),
        });
      }
    }

    // Write back if apply=true
    if apply && !fixed.is_empty() {
      std::fs::write(path, doc.to_string()).with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(fixed)
  }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct WorkspaceDep {
  name: String,
  spec: String,
}

/// Report of fixes applied
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixReport {
  pub total_fixed: usize,
  pub fixed: Vec<FixedIssue>,
  pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedIssue {
  pub crate_name: String,
  pub dependency_name: String,
  pub section: String,
  pub before: String,
  pub after: String,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_workspace_dep_parsing() {
    // Test that we can parse workspace.dependencies
    let toml = r#"
[workspace]
members = ["crates/*"]

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = "1.0"
"#;

    let doc: DocumentMut = toml.parse().unwrap();
    let workspace = doc.get("workspace").unwrap().as_table().unwrap();
    let deps = workspace.get("dependencies").unwrap().as_table().unwrap();

    assert!(deps.contains_key("serde"));
    assert!(deps.contains_key("tokio"));
  }

  #[test]
  fn test_inline_table_creation() {
    // Test that we can create { workspace = true }
    let mut table = toml_edit::InlineTable::new();
    table.insert("workspace", Value::Boolean(toml_edit::Formatted::new(true)));

    let item = Item::Value(Value::InlineTable(table));
    assert_eq!(item.to_string().trim(), "{ workspace = true }");
  }
}
