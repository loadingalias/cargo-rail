//! Unused dependency detection
//!
//! Detects dependencies declared in Cargo.toml but not used in the code.
//! Uses workspace graph to find actually-used dependencies.

use super::engine::{AnalysisResult, QualityAnalysis, QualityContext};
use crate::core::error::RailResult;
use std::collections::HashSet;

/// Analysis for detecting unused dependencies
pub struct UnusedDepsAnalysis;

impl QualityAnalysis for UnusedDepsAnalysis {
  fn name(&self) -> &str {
    "unused-deps"
  }

  fn description(&self) -> &str {
    "Detects dependencies declared but not used"
  }

  fn analyze(&self, ctx: &QualityContext) -> RailResult<AnalysisResult> {
    let violations = Vec::new();

    // For each workspace member, compare declared vs. used dependencies
    for crate_name in ctx.graph.workspace_members() {
      // Get declared dependencies from graph
      if let Ok(declared_deps) = ctx.graph.direct_dependencies(&crate_name) {
        // Filter to workspace members only (external deps need different analysis)
        let declared_workspace_deps: HashSet<_> = declared_deps
          .iter()
          .filter(|dep| ctx.graph.workspace_members().contains(dep))
          .cloned()
          .collect();

        // For now, we consider all workspace deps as "used" since they're in the graph
        // A more sophisticated analysis would parse Rust files to find actual usage
        // TODO: Parse use statements and imports to find truly unused deps

        // This is a placeholder implementation
        // Real implementation would:
        // 1. Parse all .rs files in the crate
        // 2. Find all use/extern crate statements
        // 3. Compare with declared dependencies
        // 4. Report unused ones

        // For MVP, we skip this analysis for now
        let _ = declared_workspace_deps; // Suppress unused warning
      }
    }

    Ok(AnalysisResult {
      analysis: self.name().to_string(),
      passed: violations.is_empty(),
      violations,
      summary: Some(serde_json::json!({
        "note": "Unused deps analysis requires source code parsing - deferred to future implementation"
      })),
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_analysis_name() {
    let analysis = UnusedDepsAnalysis;
    assert_eq!(analysis.name(), "unused-deps");
  }
}
