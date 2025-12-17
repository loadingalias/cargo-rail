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
/// 2. Single reverse traversal from all direct crates to find dependents (O(V+E))
/// 3. Union direct crates + dependents for test targets
///
/// # Performance
/// O(n + V + E) where n = files, V = vertices, E = edges.
/// Typically <50ms for <100 crates. The single traversal approach is
/// significantly faster than O(N × (V+E)) when many crates are directly affected.
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

  // Map files → crates (uses interior mutability for cache)
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

  // Get all transitive dependents in a single traversal
  // This is O(V+E) regardless of how many direct crates there are,
  // vs O(N × (V+E)) if we called transitive_dependents() for each crate
  let all_dependents = graph.transitive_dependents_of_set(&direct_crates)?;

  // Build test targets (direct + dependents)
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
