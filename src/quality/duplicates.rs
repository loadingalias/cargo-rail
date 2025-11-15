//! Duplicate dependency version detection
//!
//! Detects when multiple versions of the same dependency exist in the workspace.
//! Reuses logic from `lint::versions` but integrates with unified quality engine.

use super::engine::{AnalysisResult, QualityAnalysis, QualityContext, Violation};
use crate::core::error::RailResult;
use std::collections::HashMap;

/// Analysis for detecting duplicate dependency versions
pub struct DuplicateVersionsAnalysis;

impl QualityAnalysis for DuplicateVersionsAnalysis {
  fn name(&self) -> &str {
    "duplicate-versions"
  }

  fn description(&self) -> &str {
    "Detects duplicate versions of the same dependency"
  }

  fn analyze(&self, ctx: &QualityContext) -> RailResult<AnalysisResult> {
    let mut violations = Vec::new();

    // Group dependencies by name, collecting all versions
    let mut dep_versions: HashMap<String, Vec<(String, String)>> = HashMap::new();

    // Iterate through all packages in the metadata
    let metadata = ctx.graph.metadata();
    for package in &metadata.metadata_json().packages {
      for dep in &package.dependencies {
        let dep_name = dep.name.as_str();

        // Find the resolved version of this dependency
        if let Some(resolved_pkg) = metadata
          .metadata_json()
          .packages
          .iter()
          .find(|p| p.name.as_str() == dep_name)
        {
          let version = resolved_pkg.version.to_string();
          let dependent = package.name.as_str().to_string();

          dep_versions
            .entry(dep_name.to_string())
            .or_default()
            .push((dependent, version));
        }
      }
    }

    // Find duplicates
    for (dep_name, usages) in dep_versions {
      // Get unique versions
      let mut versions: Vec<String> = usages.iter().map(|(_, v)| v.clone()).collect();
      versions.sort();
      versions.dedup();

      if versions.len() > 1 {
        // Check if this is forbidden by policy
        let is_forbidden = ctx.config.policy.forbid_multiple_versions.contains(&dep_name);

        let severity = if is_forbidden {
          super::engine::Severity::Error
        } else {
          super::engine::Severity::Warning
        };

        let message = format!(
          "Dependency '{}' has {} versions: {}",
          dep_name,
          versions.len(),
          versions.join(", ")
        );

        let suggestion = format!(
          "Align all workspace crates to use a single version of '{}'. Consider using workspace.dependencies inheritance.",
          dep_name
        );

        violations.push(Violation {
          severity,
          location: dep_name.clone(),
          message,
          suggestion: Some(suggestion),
          metadata: Some(serde_json::json!({
            "dependency": dep_name,
            "versions": versions,
            "forbidden_by_policy": is_forbidden,
          })),
        });
      }
    }

    Ok(AnalysisResult {
      analysis: self.name().to_string(),
      passed: violations.iter().all(|v| v.severity != super::engine::Severity::Error),
      violations,
      summary: None,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_analysis_name() {
    let analysis = DuplicateVersionsAnalysis;
    assert_eq!(analysis.name(), "duplicate-versions");
  }
}
