//! Affected crate analysis
//!
//! Given a set of changed files, determine:
//! - Which crates directly contain those files
//! - Which crates transitively depend on the changed crates
//! - Minimal set of crates that need testing

use super::core::WorkspaceGraph;
use crate::error::RailResult;
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
