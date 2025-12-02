//! Transitive dependency fragmentation detection and pinning
//!
//! Detects transitive dependencies that are resolved to different versions
//! across targets and generates pins to unify them.

use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::unify_types::TransitivePin;
use crate::progress;

/// Plans transitive dependency pinning
pub struct TransitivePlanner<'a> {
  metadata: &'a MultiTargetMetadata,
}

impl<'a> TransitivePlanner<'a> {
  /// Create a new transitive planner
  pub fn new(metadata: &'a MultiTargetMetadata) -> Self {
    Self { metadata }
  }

  /// Find fragmented transitive dependencies and generate pins
  ///
  /// Returns a list of transitive dependencies that should be pinned
  /// to unify versions across targets.
  pub fn find_pins(&self) -> Vec<TransitivePin> {
    progress!("Analyzing transitive dependencies...");
    let fragmented = self.metadata.find_fragmented_transitives();

    fragmented
      .into_iter()
      .map(|f| TransitivePin {
        name: f.name,
        version: f.version,
        features: f.unified_features,
      })
      .collect()
  }
}
