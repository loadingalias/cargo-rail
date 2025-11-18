//! Graph-aware change impact analysis
//!
//! This module provides intelligent change detection by combining:
//! - Git file-level changes
//! - Workspace dependency graph
//! - Cargo package metadata
//!
//! This enables smarter decisions about what needs to be rebuilt, retested, or synced.

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use std::collections::HashSet;
use std::path::PathBuf;

/// Categorization of changed files by type
#[derive(Debug, Default)]
pub struct ChangeCategories {
  pub source_files: Vec<PathBuf>,
  pub test_files: Vec<PathBuf>,
  pub doc_files: Vec<PathBuf>,
  pub config_files: Vec<PathBuf>,
  pub build_scripts: Vec<PathBuf>,
  pub other_files: Vec<PathBuf>,
}

impl ChangeCategories {
  /// Check if only documentation files changed
  pub fn is_docs_only(&self) -> bool {
    self.source_files.is_empty()
      && self.test_files.is_empty()
      && self.config_files.is_empty()
      && self.build_scripts.is_empty()
      && !self.doc_files.is_empty()
  }

  /// Check if any source code changed (requires rebuild)
  pub fn has_source_changes(&self) -> bool {
    !self.source_files.is_empty() || !self.build_scripts.is_empty()
  }

  /// Check if any tests changed (requires re-test)
  pub fn has_test_changes(&self) -> bool {
    !self.test_files.is_empty()
  }

  /// Check if configuration changed (may affect dependencies)
  pub fn has_config_changes(&self) -> bool {
    !self.config_files.is_empty()
  }
}

/// Comprehensive change impact report
#[derive(Debug)]
pub struct ImpactReport {
  /// All changed files with their change types (A/M/D)
  #[allow(dead_code)] // Public API field
  pub changed_files: Vec<(PathBuf, char)>,

  /// Workspace crates directly containing changed files
  pub direct_crates: Vec<String>,

  /// Crates transitively depending on direct_crates
  pub transitive_crates: Vec<String>,

  /// Categorization of changes
  pub categories: ChangeCategories,

  /// Whether changes require rebuild
  #[allow(dead_code)] // Public API field
  pub requires_rebuild: bool,

  /// Whether changes require re-testing
  #[allow(dead_code)] // Public API field
  pub requires_retest: bool,
}

impl ImpactReport {
  /// Get complete set of affected crates (direct + transitive)
  pub fn all_affected_crates(&self) -> Vec<String> {
    let mut all: HashSet<String> = self.direct_crates.iter().cloned().collect();
    all.extend(self.transitive_crates.iter().cloned());
    let mut result: Vec<_> = all.into_iter().collect();
    result.sort();
    result
  }

  /// Check if a specific crate is affected
  pub fn affects_crate(&self, crate_name: &str) -> bool {
    self.direct_crates.contains(&crate_name.to_string()) || self.transitive_crates.contains(&crate_name.to_string())
  }

  /// Get minimal test set (only affected crates)
  pub fn minimal_test_set(&self) -> Vec<String> {
    self.all_affected_crates()
  }
}

/// Graph-aware change impact analyzer
pub struct ChangeImpact<'a> {
  ctx: &'a WorkspaceContext,
}

impl<'a> ChangeImpact<'a> {
  /// Create new analyzer from workspace context
  pub fn new(ctx: &'a WorkspaceContext) -> Self {
    Self { ctx }
  }

  /// Analyze changes between two git refs with full graph awareness
  pub fn analyze_changes(&self, from: &str, to: &str) -> RailResult<ImpactReport> {
    // 1. Get changed files from git
    let changed_files = self.ctx.git.git().get_changed_files_between(from, to)?;

    // 2. Categorize changes by file type
    let categories = self.categorize_changes(&changed_files);

    // 3. Use graph to find affected crates
    let file_paths: Vec<PathBuf> = changed_files.iter().map(|(p, _)| p.clone()).collect();
    let analysis = crate::graph::analyze(&self.ctx.graph, &file_paths)?;

    // 4. Determine rebuild/retest requirements
    let requires_rebuild = categories.has_source_changes() || categories.has_config_changes();
    let requires_retest = requires_rebuild || categories.has_test_changes();

    // Convert HashSets to Vecs
    let mut direct_crates: Vec<String> = analysis.impact.direct.into_iter().collect();
    direct_crates.sort();

    let mut transitive_crates: Vec<String> = analysis.impact.dependents.into_iter().collect();
    transitive_crates.sort();

    Ok(ImpactReport {
      changed_files,
      direct_crates,
      transitive_crates,
      categories,
      requires_rebuild,
      requires_retest,
    })
  }

  /// Analyze changes for a specific crate only
  pub fn analyze_crate_changes(&self, crate_name: &str, from: &str, to: &str) -> RailResult<Option<ImpactReport>> {
    let full_report = self.analyze_changes(from, to)?;

    // Filter to only this crate
    if !full_report.affects_crate(crate_name) {
      return Ok(None);
    }

    Ok(Some(full_report))
  }

  /// Categorize changed files by type
  fn categorize_changes(&self, files: &[(PathBuf, char)]) -> ChangeCategories {
    let mut categories = ChangeCategories::default();

    for (path, _change_type) in files {
      let path_str = path.to_string_lossy();

      // Categorize by file type and location (order matters - most specific first)
      if path_str.ends_with("build.rs") {
        categories.build_scripts.push(path.clone());
      } else if path_str.ends_with("Cargo.toml") || path_str.ends_with("Cargo.lock") {
        categories.config_files.push(path.clone());
      } else if path_str.ends_with(".md") || path_str.ends_with(".txt") {
        categories.doc_files.push(path.clone());
      } else if path_str.ends_with(".rs") {
        if path_str.contains("/tests/")
          || path_str.contains("/benches/")
          || path_str.starts_with("tests/")
          || path_str.starts_with("benches/")
        {
          categories.test_files.push(path.clone());
        } else {
          categories.source_files.push(path.clone());
        }
      } else {
        categories.other_files.push(path.clone());
      }
    }

    categories
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn create_test_context() -> WorkspaceContext {
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceContext::build(&current_dir).unwrap()
  }

  #[test]
  fn test_change_categories_docs_only() {
    let mut cat = ChangeCategories::default();
    cat.doc_files.push(PathBuf::from("README.md"));

    assert!(cat.is_docs_only());
    assert!(!cat.has_source_changes());
    assert!(!cat.has_test_changes());
  }

  #[test]
  fn test_change_categories_source_changes() {
    let mut cat = ChangeCategories::default();
    cat.source_files.push(PathBuf::from("src/lib.rs"));

    assert!(!cat.is_docs_only());
    assert!(cat.has_source_changes());
  }

  #[test]
  fn test_change_impact_analyzer_creation() {
    let ctx = create_test_context();
    let _analyzer = ChangeImpact::new(&ctx);
  }

  #[test]
  fn test_categorize_rust_files() {
    let ctx = create_test_context();
    let analyzer = ChangeImpact::new(&ctx);

    let files = vec![
      (PathBuf::from("src/lib.rs"), 'M'),
      (PathBuf::from("tests/integration.rs"), 'M'),
      (PathBuf::from("README.md"), 'M'),
      (PathBuf::from("Cargo.toml"), 'M'),
      (PathBuf::from("build.rs"), 'M'),
    ];

    let cat = analyzer.categorize_changes(&files);

    assert_eq!(
      cat.source_files.len(),
      1,
      "Expected 1 source file (src/lib.rs), got: {:?}",
      cat.source_files
    );
    assert_eq!(
      cat.test_files.len(),
      1,
      "Expected 1 test file (tests/integration.rs), got: {:?}",
      cat.test_files
    );
    assert_eq!(
      cat.doc_files.len(),
      1,
      "Expected 1 doc file (README.md), got: {:?}",
      cat.doc_files
    );
    assert_eq!(
      cat.config_files.len(),
      1,
      "Expected 1 config file (Cargo.toml), got: {:?}",
      cat.config_files
    );
    assert_eq!(
      cat.build_scripts.len(),
      1,
      "Expected 1 build script (build.rs), got: {:?}",
      cat.build_scripts
    );
  }
}
