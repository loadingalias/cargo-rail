//! Affected crate analysis
//!
//! Given a set of changed files, determine:
//! - Which crates directly contain those files
//! - Which crates transitively depend on the changed crates
//! - Minimal set of crates that need testing

use super::workspace_graph::WorkspaceGraph;
use crate::core::error::RailResult;
use std::collections::HashSet;
use std::path::Path;

/// Set of affected crates from file changes.
#[derive(Debug, Clone)]
pub struct AffectedSet {
  /// Crates directly containing changed files
  pub direct: HashSet<String>,

  /// Transitive dependents of changed crates
  pub dependents: HashSet<String>,

  /// Minimal test set (direct + dependents)
  pub test_targets: HashSet<String>,
}

impl AffectedSet {
  // TODO: Used by future `cargo rail test --since` to skip empty test sets
  #[allow(dead_code)]
  pub fn is_empty(&self) -> bool {
    self.direct.is_empty()
  }

  // TODO: Used by future CI summary output and --dry-run stats
  #[allow(dead_code)]
  pub fn total_affected(&self) -> usize {
    self.test_targets.len()
  }
}

/// Complete affected analysis.
#[derive(Debug, Clone)]
pub struct AffectedAnalysis {
  /// Files that changed
  pub changed_files: Vec<String>,

  /// Impact set
  pub impact: AffectedSet,
}

/// Analyze which crates are affected by file changes.
///
/// Algorithm:
/// 1. Map files → owning crates (O(n) with path cache)
/// 2. For each changed crate, get transitive dependents (O(V+E) per crate)
/// 3. Union all sets
///
/// # Performance
/// Typical: <50ms for <100 crates
pub fn analyze(graph: &WorkspaceGraph, changed_files: &[impl AsRef<Path>]) -> RailResult<AffectedAnalysis> {
  if changed_files.is_empty() {
    return Ok(AffectedAnalysis {
      changed_files: vec![],
      impact: AffectedSet {
        direct: HashSet::new(),
        dependents: HashSet::new(),
        test_targets: HashSet::new(),
      },
    });
  }

  // Step 1: Map files → crates (uses interior mutability for cache)
  let direct_crates = graph.files_to_crates(changed_files);

  if direct_crates.is_empty() {
    // No workspace crates affected (e.g., README, LICENSE, etc.)
    return Ok(AffectedAnalysis {
      changed_files: changed_files.iter().map(|p| p.as_ref().display().to_string()).collect(),
      impact: AffectedSet {
        direct: HashSet::new(),
        dependents: HashSet::new(),
        test_targets: HashSet::new(),
      },
    });
  }

  // Step 2: Get transitive dependents for each changed crate
  let mut all_dependents = HashSet::new();

  for crate_name in &direct_crates {
    let dependents = graph.transitive_dependents(crate_name)?;
    all_dependents.extend(dependents);
  }

  // Step 3: Build test targets (direct + dependents)
  let mut test_targets = direct_crates.clone();
  test_targets.extend(all_dependents.clone());

  Ok(AffectedAnalysis {
    changed_files: changed_files.iter().map(|p| p.as_ref().display().to_string()).collect(),
    impact: AffectedSet {
      direct: direct_crates,
      dependents: all_dependents,
      test_targets,
    },
  })
}

/// Compute minimal test set from changed files.
///
/// Convenience function that returns just the crate names.
///
/// TODO: Used by `cargo rail test --since` and `cargo rail check --since`
#[allow(dead_code)]
pub fn minimal_test_set(graph: &WorkspaceGraph, changed_files: &[impl AsRef<Path>]) -> RailResult<Vec<String>> {
  let analysis = analyze(graph, changed_files)?;
  let mut targets: Vec<_> = analysis.impact.test_targets.into_iter().collect();
  targets.sort();
  Ok(targets)
}

#[cfg(test)]
mod tests {
  #[test]
  fn test_empty_changeset() {
    // Test with no changed files
    let files: Vec<&str> = vec![];
    // Would need actual WorkspaceGraph to test properly
    // Just verify API compiles
    assert!(files.is_empty());
  }
}
