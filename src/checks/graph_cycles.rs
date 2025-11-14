//! Dependency cycle detection check
//!
//! Validates that the workspace dependency graph is acyclic using Tarjan's SCC algorithm.
//! Cycles in Rust dependencies are compile errors, so this catches configuration issues early.

use super::trait_def::{Check, CheckContext, CheckResult, Severity};
use crate::core::error::RailResult;
use crate::graph::WorkspaceGraph;

/// Check for dependency cycles in the workspace
pub struct GraphCyclesCheck;

impl Check for GraphCyclesCheck {
  fn name(&self) -> &'static str {
    "graph-cycles"
  }

  fn description(&self) -> &'static str {
    "Detect dependency cycles in workspace crates"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // Load workspace graph
    let graph = match WorkspaceGraph::load(&ctx.workspace_root) {
      Ok(g) => g,
      Err(e) => {
        return Ok(CheckResult {
          check_name: self.name().to_string(),
          passed: false,
          severity: Severity::Error,
          message: format!("Failed to load workspace graph: {}", e),
          suggestion: Some("Run `cargo metadata` to check workspace validity".to_string()),
          details: None,
        });
      }
    };

    // Detect cycles
    let cycles = graph.find_cycles();

    if cycles.is_empty() {
      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: true,
        severity: Severity::Info,
        message: "No dependency cycles detected".to_string(),
        suggestion: None,
        details: None,
      })
    } else {
      let cycle_list: Vec<String> = cycles
        .iter()
        .enumerate()
        .map(|(i, cycle)| format!("Cycle {}: {}", i + 1, cycle.join(" → ")))
        .collect();

      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: false,
        severity: Severity::Error,
        message: format!("Found {} dependency cycle(s) in workspace", cycles.len()),
        suggestion: Some("Review and remove circular dependencies between crates".to_string()),
        details: Some(serde_json::json!({ "cycles": cycle_list })),
      })
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_check_name() {
    let check = GraphCyclesCheck;
    assert_eq!(check.name(), "graph-cycles");
  }

  #[test]
  fn test_check_on_cargo_rail() {
    // Test on cargo-rail itself (should have no cycles)
    let ctx = CheckContext {
      workspace_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
      crate_name: None,
      thorough: false,
    };

    let check = GraphCyclesCheck;
    let result = check.run(&ctx).unwrap();

    // cargo-rail should have no cycles
    assert!(result.passed, "cargo-rail workspace should have no cycles");
  }
}
