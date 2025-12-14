//! Dependency iteration strategies for unification
//!
//! Encapsulates the `include_renamed` branching logic into a clean iterator pattern.
//! - `ByDepKey` mode: Each dep_key is processed separately
//! - `ByPackage` mode: Renamed variants are aggregated by package name

use crate::cargo::manifest_analyzer::{DepKey, DepUsage, ManifestAnalyzer};
use crate::config::UnifyConfig;
use std::collections::HashSet;
use std::sync::Arc;

/// A dependency candidate for unification
pub struct UnifyCandidate<'a> {
  /// The dep_key (for ByDepKey mode, or the first dep_key for ByPackage mode)
  pub dep_key: &'a DepKey,
  /// All usage sites for this candidate
  pub usages: Vec<&'a DepUsage>,
}

/// Iterator that yields unification candidates based on configuration
pub struct CandidateIterator<'a> {
  manifests: &'a ManifestAnalyzer,
  config: &'a UnifyConfig,
  /// Track processed packages (for ByPackage deduplication) - uses `Arc<str>` for cheap clones
  processed: HashSet<Arc<str>>,
  /// All dep_keys to iterate over
  all_deps: Vec<&'a DepKey>,
  /// Current index into all_deps
  index: usize,
}

impl<'a> CandidateIterator<'a> {
  /// Create a new candidate iterator
  pub fn new(manifests: &'a ManifestAnalyzer, config: &'a UnifyConfig) -> Self {
    let all_deps = manifests.all_dependencies();
    Self {
      manifests,
      config,
      processed: HashSet::new(),
      all_deps,
      index: 0,
    }
  }

  /// Check if a candidate should be skipped based on filtering rules
  fn should_skip(&self, dep_key: &DepKey, usage_count: usize) -> bool {
    // Skip if excluded (compare &str to avoid allocation)
    if self.config.exclude.iter().any(|s| s.as_str() == &*dep_key.name) {
      return true;
    }

    // Skip if usage count too low (unless in include list)
    if usage_count < 2 && !self.config.include.iter().any(|s| s.as_str() == &*dep_key.name) {
      return true;
    }

    // Skip renamed deps in ByDepKey mode
    if dep_key.renamed_from.is_some() && !self.config.include_renamed {
      return true;
    }

    false
  }
}

impl<'a> Iterator for CandidateIterator<'a> {
  type Item = UnifyCandidate<'a>;

  fn next(&mut self) -> Option<Self::Item> {
    while self.index < self.all_deps.len() {
      let dep_key = self.all_deps[self.index];
      self.index += 1;

      // ByPackage mode (include_renamed = true)
      if self.config.include_renamed {
        // Skip if already processed this package
        if self.processed.contains(&dep_key.name) {
          continue;
        }

        let usage_count = self.manifests.package_usage_count(&dep_key.name);

        if self.should_skip(dep_key, usage_count) {
          continue;
        }

        // Mark as processed (cheap Arc clone)
        self.processed.insert(Arc::clone(&dep_key.name));

        // Get all usage sites for this package (aggregated across variants)
        let usages = self.manifests.get_package_usage_sites(&dep_key.name);

        return Some(UnifyCandidate { dep_key, usages });
      } else {
        // ByDepKey mode (include_renamed = false)
        let usage_count = self.manifests.usage_count(dep_key);

        if self.should_skip(dep_key, usage_count) {
          continue;
        }

        let usages = self.manifests.get_usage_sites(dep_key);

        return Some(UnifyCandidate { dep_key, usages });
      }
    }

    None
  }
}
