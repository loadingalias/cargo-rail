//! Presentation layer for change detection
//!
//! Layer 3 of the change detection system:
//! - Layer 1: `classify.rs` - Pure file classification (ChangeKind)
//! - Layer 2: `workspace::change_analyzer` - Graph + cargo + proc-macro aware (ImpactReport)
//! - Layer 3: This module - Presentation choices (docs-only, infra, custom categories)
//!
//! This layer applies user-configurable rules (from ChangeDetectionConfig) to classify
//! changed files for presentation to the user or CI systems.

use crate::change_detection::classify::{
  classify_path, compile_custom_patterns, compile_infrastructure_patterns, custom_surfaces_for_path,
  matches_infrastructure_patterns,
};
use crate::config::ChangeDetectionConfig;
use rustc_hash::FxHashMap;
use std::path::Path;

/// Presentation-level classification of changed files
///
/// Used for determining special display modes (docs-only, rebuild-all)
/// and matching custom categories from configuration.
#[derive(Debug, Clone, Default)]
pub struct ChangeClassification {
  /// True if only documentation files changed (no tests required)
  pub docs_only: bool,
  /// True if infrastructure files changed (requires full rebuild)
  pub rebuild_all: bool,
  /// Infrastructure files that triggered rebuild_all
  pub infrastructure_files: Vec<String>,
  /// Custom category matches: category name -> matched files
  pub custom_categories: FxHashMap<String, Vec<String>>,
}

impl ChangeClassification {
  /// Check if this classification indicates no testing is required
  pub fn skip_tests(&self) -> bool {
    self.docs_only
  }

  /// Check if this classification indicates a full rebuild is needed
  pub fn needs_full_rebuild(&self) -> bool {
    self.rebuild_all
  }

  /// Get all custom category names that have matches
  pub fn matched_categories(&self) -> Vec<&str> {
    self.custom_categories.keys().map(|s| s.as_str()).collect()
  }
}

/// Classifier for presentation-layer change detection
///
/// Applies configuration rules to classify changes for display/CI integration.
pub struct ChangeClassifier {
  /// Compiled infrastructure patterns
  infra_patterns: Vec<glob::Pattern>,
  /// Compiled custom category patterns: (category_name, patterns)
  custom_patterns: Vec<(String, glob::Pattern)>,
}

impl ChangeClassifier {
  /// Create a new classifier from configuration
  pub fn new(config: &ChangeDetectionConfig) -> Self {
    Self {
      infra_patterns: compile_infrastructure_patterns(Some(config)),
      custom_patterns: compile_custom_patterns(Some(config)),
    }
  }

  /// Create a classifier with default configuration
  pub fn default_config() -> Self {
    Self::new(&ChangeDetectionConfig::default())
  }

  /// Classify a list of changed file paths
  ///
  /// Produces a [`ChangeClassification`] with docs-only/full-rebuild signals,
  /// triggering infrastructure files, and matched custom categories.
  pub fn classify<P: AsRef<Path>>(&self, files: &[P]) -> ChangeClassification {
    let mut result = ChangeClassification::default();
    let mut has_non_doc_changes = false;

    for file in files {
      let path = file.as_ref();
      let path_str = path.to_string_lossy();
      let profile = classify_path(path);
      let builtin_infra = profile.default_surfaces().contains(&"infra");
      let configured_infra = matches_infrastructure_patterns(&path_str, &self.infra_patterns);
      let effective_infra = builtin_infra || configured_infra;

      if effective_infra {
        result.rebuild_all = true;
        if !result
          .infrastructure_files
          .iter()
          .any(|existing| existing == path_str.as_ref())
        {
          result.infrastructure_files.push(path_str.to_string());
        }
        has_non_doc_changes = true;
      }

      for surface in custom_surfaces_for_path(&path_str, &self.custom_patterns) {
        let Some(category) = surface.strip_prefix("custom:") else {
          continue;
        };
        let matches = result.custom_categories.entry(category.to_string()).or_default();
        if !matches.iter().any(|existing| existing == path_str.as_ref()) {
          matches.push(path_str.to_string());
        }
      }

      if !profile.is_docs_only() {
        has_non_doc_changes = true;
      }
    }

    // Set docs_only if we have changes and they're all docs
    result.docs_only = !files.is_empty() && !has_non_doc_changes;

    result
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  fn default_classifier() -> ChangeClassifier {
    ChangeClassifier::default_config()
  }

  #[test]
  fn test_docs_only() {
    let classifier = default_classifier();

    let files = vec![PathBuf::from("README.md"), PathBuf::from("docs/guide.md")];
    let result = classifier.classify(&files);

    assert!(result.docs_only, "Should be docs-only");
    assert!(!result.rebuild_all, "Should not require rebuild");
    assert!(result.infrastructure_files.is_empty());
  }

  #[test]
  fn test_docs_only_with_source() {
    let classifier = default_classifier();

    let files = vec![PathBuf::from("README.md"), PathBuf::from("src/lib.rs")];
    let result = classifier.classify(&files);

    assert!(!result.docs_only, "Should not be docs-only with source file");
  }

  #[test]
  fn test_infrastructure_files() {
    let classifier = default_classifier();

    let files = vec![PathBuf::from(".github/workflows/ci.yml")];
    let result = classifier.classify(&files);

    assert!(result.rebuild_all, "Should require rebuild for CI changes");
    assert_eq!(result.infrastructure_files.len(), 1);
  }

  #[test]
  fn test_custom_categories() {
    let mut config = ChangeDetectionConfig::default();
    config
      .custom
      .insert("verify".to_string(), vec!["verify/**/*.rs".to_string()]);

    let classifier = ChangeClassifier::new(&config);

    let files = vec![PathBuf::from("verify/tests/model.rs")];
    let result = classifier.classify(&files);

    assert!(result.custom_categories.contains_key("verify"));
    assert_eq!(result.custom_categories["verify"].len(), 1);
  }

  #[test]
  fn test_infrastructure_file_can_also_match_custom_category() {
    let mut config = ChangeDetectionConfig {
      infrastructure: vec![".github/**".to_string()],
      ..Default::default()
    };
    config
      .custom
      .insert("merge_validation".to_string(), vec![".github/workflows/**".to_string()]);

    let classifier = ChangeClassifier::new(&config);

    let files = vec![PathBuf::from(".github/workflows/ci.yml")];
    let result = classifier.classify(&files);

    assert!(result.rebuild_all, "workflow changes should still require rebuild_all");
    assert_eq!(
      result.infrastructure_files,
      vec![".github/workflows/ci.yml".to_string()]
    );
    assert!(
      result.custom_categories.contains_key("merge_validation"),
      "workflow changes should also match custom merge-validation routing"
    );
  }

  #[test]
  fn test_empty_files() {
    let classifier = default_classifier();

    let files: Vec<PathBuf> = vec![];
    let result = classifier.classify(&files);

    assert!(!result.docs_only, "Empty file list should not be docs-only");
    assert!(!result.rebuild_all);
  }

  #[test]
  fn test_justfile_is_infrastructure() {
    let classifier = default_classifier();

    let files = vec![PathBuf::from("justfile")];
    let result = classifier.classify(&files);

    assert!(result.rebuild_all, "justfile should trigger rebuild_all");
  }
}
