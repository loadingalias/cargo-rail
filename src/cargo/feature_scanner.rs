//! Dead feature detection using resolved cargo metadata
//!
//! Detects features declared in workspace crates that are never enabled
//! by any consumer in the resolved dependency graph across all target triples.
//!
//! This module distinguishes between:
//! - **Truly dead features**: Empty no-ops (`simd-accel = []`) - safe to remove
//! - **Optional features**: Enable deps/features but not currently used - NOT safe to remove
//!
//! Safety checks before marking a feature as dead:
//! 1. Not enabled in resolved dependency graph
//! 2. Not referenced by other features in `[features]` table (internal refs)
//! 3. Not referenced by other crates via `dep/feature` syntax (external refs)
//! 4. Not used in source code via `#[cfg(feature = "...")]` (conditional compilation)

use cargo_metadata::Package;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::multi_target_metadata::MultiTargetMetadata;

/// Scan source files for `cfg(feature = "...")` patterns
///
/// Returns a set of feature names referenced in the source code.
/// This prevents pruning features that are used for conditional compilation.
fn scan_source_for_cfg_features(crate_dir: &Path) -> HashSet<String> {
  let mut features = HashSet::new();
  let src_dir = crate_dir.join("src");

  if !src_dir.exists() {
    return features;
  }

  // Walk the src directory for .rs files
  if let Ok(entries) = glob::glob(&format!("{}/**/*.rs", src_dir.display())) {
    for entry in entries.flatten() {
      if let Ok(content) = fs::read_to_string(&entry) {
        extract_cfg_features(&content, &mut features);
      }
    }
  }

  features
}

/// Extract feature names from `feature = "name"` patterns in source code
///
/// Handles patterns like:
/// - `#[cfg(feature = "foo")]`
/// - `#[cfg(not(feature = "foo"))]`
/// - `#[cfg_attr(feature = "foo", ...)]`
/// - `cfg!(feature = "foo")`
fn extract_cfg_features(content: &str, features: &mut HashSet<String>) {
  // Look for `feature = "` or `feature="` patterns
  let mut remaining = content;

  while let Some(idx) = remaining.find("feature") {
    remaining = &remaining[idx + 7..]; // Skip past "feature"

    // Skip whitespace
    let trimmed = remaining.trim_start();

    // Check for `=`
    if !trimmed.starts_with('=') {
      continue;
    }
    let after_eq = trimmed[1..].trim_start();

    // Check for opening quote
    if !after_eq.starts_with('"') {
      continue;
    }
    let after_quote = &after_eq[1..];

    // Find closing quote
    if let Some(end_quote) = after_quote.find('"') {
      let feature_name = &after_quote[..end_quote];
      if !feature_name.is_empty() && is_valid_feature_name(feature_name) {
        features.insert(feature_name.to_string());
      }
    }
  }
}

/// Check if a string is a valid Cargo feature name
fn is_valid_feature_name(s: &str) -> bool {
  !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Result of analyzing a crate's features
#[derive(Debug, Clone)]
pub struct FeatureScanResult {
  /// Crate name that was analyzed
  pub crate_name: String,
  /// Features declared in Cargo.toml
  pub declared_features: HashSet<String>,
  /// Features that are actually enabled in the resolved graph
  pub enabled_features: HashSet<String>,
  /// Truly dead features: empty no-ops that can be safely removed
  /// These are features with empty definitions (`feature = []`) that are never enabled
  pub dead_features: HashSet<String>,
  /// Optional features: not enabled but enable something (deps, other features)
  /// These are user-facing API and should NOT be removed, only reported
  pub optional_features: HashSet<String>,
}

/// Analyzes workspace features using resolved cargo metadata
pub struct FeatureScanner;

impl FeatureScanner {
  /// Analyze a single workspace member for dead and optional features
  ///
  /// Uses the resolved metadata across all targets to determine which
  /// declared features are never enabled.
  ///
  /// A feature is considered "alive" (not dead/optional) if:
  /// 1. It's enabled in the resolved graph for any target, OR
  /// 2. It's referenced by any workspace crate's feature definition (e.g., `dep/feature`), OR
  /// 3. It's referenced by another feature in the same crate (e.g., `production = ["api-key"]`), OR
  /// 4. It's used in source code via `#[cfg(feature = "...")]` (conditional compilation)
  ///
  /// For features that are NOT alive, we distinguish:
  /// - **Dead features**: Empty no-ops (`feature = []`) - safe to remove
  /// - **Optional features**: Enable something (deps, other features) - user-facing API, don't remove
  pub fn analyze_crate(
    pkg: &Package,
    metadata: &MultiTargetMetadata,
    referenced_features: &HashMap<String, HashSet<String>>,
  ) -> FeatureScanResult {
    let crate_name = pkg.name.to_string();

    // Get declared features from the package
    let declared_features: HashSet<String> = pkg.features.keys().cloned().collect();

    // Get enabled features across all targets from the resolved graph
    let enabled_across_targets = metadata.all_features(&crate_name);
    let enabled_features: HashSet<String> = enabled_across_targets.values().flatten().cloned().collect();

    // Get features referenced by other workspace crates (even if not currently enabled)
    let externally_referenced = referenced_features.get(&crate_name).cloned().unwrap_or_default();

    // Build set of features referenced internally by other features in the same crate
    // e.g., if `production = ["api-key", "server"]`, then "api-key" is internally referenced
    let internally_referenced = Self::build_internally_referenced_features(pkg);

    // Scan source code for cfg(feature = "...") patterns
    // This prevents pruning features used for conditional compilation
    let crate_dir = pkg.manifest_path.parent().map(Path::new).unwrap_or(Path::new("."));
    let source_referenced = scan_source_for_cfg_features(crate_dir);

    // Find unused features (not enabled, not externally referenced, not internally referenced,
    // not source referenced, not "default")
    let unused_features: HashSet<String> = declared_features
      .difference(&enabled_features)
      .filter(|f| *f != "default")
      .filter(|f| !externally_referenced.contains(*f))
      .filter(|f| !internally_referenced.contains(*f))
      .filter(|f| !source_referenced.contains(*f))
      .cloned()
      .collect();

    // Separate unused features into dead (empty) vs optional (enable something)
    let mut dead_features = HashSet::new();
    let mut optional_features = HashSet::new();

    for feature in &unused_features {
      if let Some(enables) = pkg.features.get(feature) {
        if enables.is_empty() {
          // Feature enables nothing - truly dead, safe to remove
          dead_features.insert(feature.clone());
        } else {
          // Feature enables deps/other features - optional user-facing API
          optional_features.insert(feature.clone());
        }
      }
    }

    FeatureScanResult {
      crate_name,
      declared_features,
      enabled_features,
      dead_features,
      optional_features,
    }
  }

  /// Analyze all workspace members for dead and optional features
  ///
  /// Returns results for crates that have dead or optional features.
  pub fn analyze_workspace(metadata: &MultiTargetMetadata) -> Vec<FeatureScanResult> {
    // First, build a map of all features referenced by workspace crates
    // This catches conditional features like `dep/feature` in [features] tables
    let referenced_features = Self::build_referenced_features_map(metadata);

    let mut results = Vec::new();

    for pkg in metadata.workspace_packages() {
      // Skip crates with no declared features
      if pkg.features.is_empty() {
        continue;
      }

      let result = Self::analyze_crate(pkg, metadata, &referenced_features);

      // Include if there are dead OR optional features
      if !result.dead_features.is_empty() || !result.optional_features.is_empty() {
        results.push(result);
      }
    }

    results
  }

  /// Build a map of features referenced by workspace crates' feature definitions
  ///
  /// Scans all workspace packages' `[features]` tables to find references like:
  /// - `dep/feature` (enables `feature` in dependency `dep`)
  /// - `dep?/feature` (optional dep feature)
  ///
  /// Returns: HashMap<crate_name, HashSet<feature_name>>
  /// where the features are ones referenced externally by other crates.
  fn build_referenced_features_map(metadata: &MultiTargetMetadata) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();

    for pkg in metadata.workspace_packages() {
      // Scan all feature definitions in this package
      for feature_deps in pkg.features.values() {
        for dep_str in feature_deps {
          // Parse feature references like "dep/feature" or "dep?/feature"
          if let Some((dep_name, feature_name)) = Self::parse_feature_reference(dep_str) {
            map.entry(dep_name).or_default().insert(feature_name);
          }
        }
      }
    }

    map
  }

  /// Build a set of features that are referenced by other features in the same crate
  ///
  /// For example, if `[features]` contains:
  /// ```toml
  /// production = ["api-key", "server"]
  /// ```
  /// Then "api-key" and "server" are internally referenced and should not be pruned
  /// (even if they are empty), because removing them would break the "production" feature.
  fn build_internally_referenced_features(pkg: &Package) -> HashSet<String> {
    let mut referenced = HashSet::new();

    for feature_deps in pkg.features.values() {
      for dep_str in feature_deps {
        // Skip dep/feature references (handled elsewhere) and dep:dep references
        if dep_str.contains('/') || dep_str.starts_with("dep:") {
          continue;
        }
        // This is a plain feature name reference within the same crate
        referenced.insert(dep_str.clone());
      }
    }

    referenced
  }

  /// Parse a feature reference string to extract dep name and feature
  ///
  /// Handles formats:
  /// - `dep/feature` -> Some(("dep", "feature"))
  /// - `dep?/feature` -> Some(("dep", "feature"))
  /// - `dep:feature` (older format) -> Some(("dep", "feature"))
  /// - `feature` (just a feature name) -> None
  /// - `dep:dep` (enabling optional dep) -> None
  fn parse_feature_reference(s: &str) -> Option<(String, String)> {
    // Try slash format first (most common): dep/feature or dep?/feature
    if let Some(idx) = s.find('/') {
      let dep_part = &s[..idx];
      let feature = &s[idx + 1..];
      // Remove trailing ? from optional deps
      let dep_name = dep_part.trim_end_matches('?');
      if !feature.is_empty() {
        return Some((dep_name.to_string(), feature.to_string()));
      }
    }

    None
  }

  /// Get total count of dead features across workspace
  pub fn count_dead_features(results: &[FeatureScanResult]) -> usize {
    results.iter().map(|r| r.dead_features.len()).sum()
  }

  /// Get total count of optional features across workspace
  pub fn count_optional_features(results: &[FeatureScanResult]) -> usize {
    results.iter().map(|r| r.optional_features.len()).sum()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_feature_scan_result_dead_features() {
    let mut declared = HashSet::new();
    declared.insert("foo".to_string());
    declared.insert("bar".to_string());
    declared.insert("default".to_string());

    let mut enabled = HashSet::new();
    enabled.insert("foo".to_string());

    // bar is dead (declared but not enabled)
    // default is NOT dead (special case)
    let dead: HashSet<String> = declared
      .difference(&enabled)
      .filter(|f| *f != "default")
      .cloned()
      .collect();

    assert!(dead.contains("bar"));
    assert!(!dead.contains("default"));
    assert!(!dead.contains("foo"));
    assert_eq!(dead.len(), 1);
  }

  #[test]
  fn test_parse_feature_reference() {
    // Standard dep/feature format
    assert_eq!(
      FeatureScanner::parse_feature_reference("tikv_alloc/mimalloc"),
      Some(("tikv_alloc".to_string(), "mimalloc".to_string()))
    );

    // Optional dep format: dep?/feature
    assert_eq!(
      FeatureScanner::parse_feature_reference("serde?/derive"),
      Some(("serde".to_string(), "derive".to_string()))
    );

    // Just a feature name (no dep reference)
    assert_eq!(FeatureScanner::parse_feature_reference("std"), None);

    // dep:dep format (enabling optional dep, not a feature)
    assert_eq!(FeatureScanner::parse_feature_reference("dep:serde"), None);

    // Empty feature after slash
    assert_eq!(FeatureScanner::parse_feature_reference("dep/"), None);
  }

  #[test]
  fn test_internally_referenced_filter() {
    // Test the filter logic for internally referenced features
    // This simulates what build_internally_referenced_features does

    let feature_deps = vec![
      // production = ["api-key", "server"]
      vec!["api-key".to_string(), "server".to_string()],
      // server = ["build"]
      vec!["build".to_string()],
      // build = ["dep/feature"] (external ref)
      vec!["dep/feature".to_string()],
      // with-serde = ["dep:serde"] (dep: syntax)
      vec!["dep:serde".to_string()],
    ];

    let mut referenced = HashSet::new();
    for deps in &feature_deps {
      for dep_str in deps {
        if dep_str.contains('/') || dep_str.starts_with("dep:") {
          continue;
        }
        referenced.insert(dep_str.clone());
      }
    }

    // api-key is referenced by production
    assert!(
      referenced.contains("api-key"),
      "api-key should be internally referenced"
    );
    // server is referenced by production
    assert!(referenced.contains("server"), "server should be internally referenced");
    // build is referenced by server
    assert!(referenced.contains("build"), "build should be internally referenced");
    // dep/feature is NOT a plain feature reference (has slash)
    assert!(!referenced.contains("dep/feature"), "dep/feature should not be in set");
    // dep:serde is NOT a plain feature reference (has dep:)
    assert!(!referenced.contains("dep:serde"), "dep:serde should not be in set");
  }

  #[test]
  fn test_extract_cfg_features_basic_patterns() {
    let mut features = HashSet::new();

    // Standard cfg attribute
    extract_cfg_features(r#"#[cfg(feature = "foo")]"#, &mut features);
    assert!(features.contains("foo"), "should detect cfg(feature = \"foo\")");

    // No spaces around =
    features.clear();
    extract_cfg_features(r#"#[cfg(feature="bar")]"#, &mut features);
    assert!(features.contains("bar"), "should detect feature=\"bar\"");

    // Extra spaces
    features.clear();
    extract_cfg_features(r#"#[cfg(feature   =   "baz")]"#, &mut features);
    assert!(features.contains("baz"), "should detect with extra spaces");

    // cfg! macro
    features.clear();
    extract_cfg_features(r#"if cfg!(feature = "qux") {"#, &mut features);
    assert!(features.contains("qux"), "should detect cfg! macro");
  }

  #[test]
  fn test_extract_cfg_features_compound_patterns() {
    let mut features = HashSet::new();

    // not(feature = "...")
    extract_cfg_features(r#"#[cfg(not(feature = "disabled"))]"#, &mut features);
    assert!(features.contains("disabled"), "should detect not(feature)");

    // all(..., feature = "...")
    features.clear();
    extract_cfg_features(r#"#[cfg(all(unix, feature = "unix-ext"))]"#, &mut features);
    assert!(features.contains("unix-ext"), "should detect in all()");

    // any(..., feature = "...")
    features.clear();
    extract_cfg_features(r#"#[cfg(any(feature = "a", feature = "b"))]"#, &mut features);
    assert!(features.contains("a"), "should detect first in any()");
    assert!(features.contains("b"), "should detect second in any()");

    // cfg_attr
    features.clear();
    extract_cfg_features(r#"#[cfg_attr(feature = "serde", derive(Serialize))]"#, &mut features);
    assert!(features.contains("serde"), "should detect in cfg_attr");
  }

  #[test]
  fn test_extract_cfg_features_real_world() {
    let mut features = HashSet::new();

    // Real example from helix-db
    let helix_code = r#"
      #[cfg(feature = "api-key")]
      match headers.get("x-api-key") {
          Some(key) => validate_key(key),
          None => return Err(AuthError),
      }
      #[cfg(not(feature = "api-key"))]
      Ok(())
    "#;
    extract_cfg_features(helix_code, &mut features);
    assert!(
      features.contains("api-key"),
      "should detect api-key from helix-db pattern"
    );

    // Real example from polars
    features.clear();
    let polars_code = r#"
      #[cfg(feature = "gather")]
      pub fn gather(&self, indices: &IdxCa) -> PolarsResult<Self> {
          // ...
      }
    "#;
    extract_cfg_features(polars_code, &mut features);
    assert!(features.contains("gather"), "should detect gather from polars pattern");

    // Real example from ruff
    features.clear();
    let ruff_code = r#"
      #[cfg(feature = "singlethreaded")]
      fn run_single() { }
      #[cfg(not(feature = "singlethreaded"))]
      fn run_parallel() { }
    "#;
    extract_cfg_features(ruff_code, &mut features);
    assert!(
      features.contains("singlethreaded"),
      "should detect singlethreaded from ruff pattern"
    );
  }

  #[test]
  fn test_extract_cfg_features_edge_cases() {
    let mut features = HashSet::new();

    // Feature with hyphen
    extract_cfg_features(r#"#[cfg(feature = "my-feature")]"#, &mut features);
    assert!(features.contains("my-feature"), "should handle hyphens");

    // Feature with underscore
    features.clear();
    extract_cfg_features(r#"#[cfg(feature = "my_feature")]"#, &mut features);
    assert!(features.contains("my_feature"), "should handle underscores");

    // Variable assignment like `let feature = "value"` technically matches our pattern
    // This is a safe false positive - we'll keep the feature rather than prune it
    // The alternative would require a full Rust parser, which is overkill
    features.clear();
    extract_cfg_features(r#"let feature = "not-a-cfg";"#, &mut features);
    // False positives are acceptable - they just prevent pruning a dead feature

    // Should NOT match: feature in a string literal
    features.clear();
    extract_cfg_features(r#"println!("feature = \"quoted\"");"#, &mut features);
    // This might match - that's acceptable, false positives are safe (we keep the feature)

    // Should NOT match: invalid feature names
    features.clear();
    extract_cfg_features(r#"#[cfg(feature = "")]"#, &mut features);
    assert!(features.is_empty(), "should not match empty feature name");
  }

  #[test]
  fn test_is_valid_feature_name() {
    // Valid names
    assert!(is_valid_feature_name("foo"));
    assert!(is_valid_feature_name("foo-bar"));
    assert!(is_valid_feature_name("foo_bar"));
    assert!(is_valid_feature_name("foo123"));
    assert!(is_valid_feature_name("FOO"));

    // Invalid names
    assert!(!is_valid_feature_name(""));
    assert!(!is_valid_feature_name("foo bar"));
    assert!(!is_valid_feature_name("foo/bar"));
    assert!(!is_valid_feature_name("foo:bar"));
  }
}
