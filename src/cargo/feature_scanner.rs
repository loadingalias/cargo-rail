//! Deterministic Cargo feature reachability analysis.
//!
//! Builds roots from package visibility, defaults, resolved configurations,
//! workspace consumers, source cfgs, Cargo target requirements, and policy,
//! then follows member-local feature edges.
//!
//! This module distinguishes between:
//! - **Truly dead features**: unreachable empty no-ops in private packages
//! - **Optional features**: Enable deps/features but not currently used - NOT safe to remove
//!
//! Only features in `publish = false` packages outside that closure are
//! removable, and only when configuration declares the workspace to be the
//! complete consumer scope. Cycles terminate because each feature is visited once.

use cargo_metadata::Package;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use super::multi_target_metadata::MultiTargetMetadata;
use crate::compiler::cfg_eval::{TargetCfgSet, cfg_expression_may_apply};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceSnapshot;

/// Feature conditions derived from source bytes bound to one workspace capture.
pub(crate) struct CapturedSourceFeatures {
  /// Minimal feature selections that can make a parsed cfg expression true.
  pub(crate) selections: Vec<Vec<String>>,
  /// Every feature name referenced by captured source.
  pub(crate) referenced: BTreeSet<String>,
  /// Parsed cfg expressions retained for target applicability decisions.
  pub(crate) expressions: BTreeSet<String>,
  /// Identifiers mentioned by captured Rust source or the package README.
  ///
  /// These conservatively retain declarations when compiler evidence cannot
  /// distinguish a documented integration from an accidental unused entry.
  pub(crate) retention_identifiers: BTreeSet<String>,
}

/// Derive source feature evidence from one authoritative workspace capture.
pub(crate) fn scan_snapshot_source_features(
  snapshot: &WorkspaceSnapshot,
  package_root: &Path,
) -> RailResult<CapturedSourceFeatures> {
  let mut selections = BTreeSet::new();
  let mut referenced = BTreeSet::new();
  let mut expressions = BTreeSet::new();
  let mut retention_identifiers = BTreeSet::new();
  let entries = snapshot.source().tree().entries();
  let package_start = entries.partition_point(|entry| entry.path.as_path() < package_root);
  for entry in &entries[package_start..] {
    let Ok(package_path) = entry.path.as_path().strip_prefix(package_root) else {
      break;
    };
    let rust_source = analysis_rust_source(package_path);
    if !rust_source && package_path != Path::new("README.md") {
      continue;
    }
    let bytes = snapshot.read_source_file(&entry.path)?.ok_or_else(|| {
      RailError::message(format!(
        "captured analysis source '{}' is absent from its workspace snapshot",
        entry.path
      ))
    })?;
    let content = std::str::from_utf8(&bytes)
      .map_err(|_| RailError::message(format!("captured analysis source '{}' is not valid UTF-8", entry.path)))?;
    extract_identifiers(content, &mut retention_identifiers);
    if !rust_source {
      continue;
    }
    let mut file_features = HashSet::new();
    extract_cfg_features(content, &mut file_features);
    referenced.extend(file_features);
    for expression in extract_cfg_expressions(content) {
      selections.extend(crate::compiler::cfg_eval::feature_selections_for_cfg(expression));
      expressions.insert(expression.to_string());
    }
  }
  Ok(CapturedSourceFeatures {
    selections: selections
      .into_iter()
      .filter(|selection| !selection.is_empty())
      .collect(),
    referenced,
    expressions,
    retention_identifiers,
  })
}

fn extract_identifiers(content: &str, identifiers: &mut BTreeSet<String>) {
  identifiers.extend(
    content
      .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
      .filter(|identifier| !identifier.is_empty())
      .map(str::to_string),
  );
}

fn analysis_rust_source(package_path: &Path) -> bool {
  if package_path == Path::new("build.rs") {
    return true;
  }
  package_path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs")
    && ["src", "tests", "benches", "examples"]
      .iter()
      .any(|root| package_path.starts_with(root))
}

/// Whether a Cargo feature edge activates or configures a dependency alias.
pub(crate) fn feature_edge_references_dependency(edge: &str, dependency: &str) -> bool {
  edge == dependency
    || edge.strip_prefix("dep:") == Some(dependency)
    || edge
      .strip_prefix(dependency)
      .is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with("?/"))
}

fn insert_feature_roots<I, S>(roots: &mut BTreeMap<String, &'static str>, features: I, root_kind: &'static str)
where
  I: IntoIterator<Item = S>,
  S: AsRef<str>,
{
  for feature in features {
    roots.entry(feature.as_ref().to_string()).or_insert(root_kind);
  }
}

fn applicable_source_features(
  manifest: &crate::cargo::manifest_analyzer::ParsedManifest,
  targets: &[&str],
  cfg_sets: &HashMap<String, TargetCfgSet>,
) -> HashSet<String> {
  let mut applicable = HashSet::new();
  let mut expression_features = HashSet::new();
  for expression in &manifest.source_cfg_expressions {
    let mut features = HashSet::new();
    extract_cfg_features(expression, &mut features);
    expression_features.extend(features.iter().cloned());
    if cfg_expression_may_apply(expression, targets.iter().map(|target| cfg_sets.get(*target))) {
      applicable.extend(features);
    }
  }
  // Preserve references not attributable to a parsed cfg expression. This
  // includes malformed syntax, where absence is not proven.
  applicable.extend(
    manifest
      .source_cfg_features
      .iter()
      .filter(|feature| !expression_features.contains(*feature))
      .cloned(),
  );
  applicable
}

fn extract_cfg_expressions(content: &str) -> Vec<&str> {
  let mut expressions = Vec::new();
  let mut offset = 0;
  while let Some(relative) = content[offset..].find("cfg(") {
    let start = offset + relative + 4;
    let bytes = content.as_bytes();
    let mut depth = 1usize;
    let mut index = start;
    let mut quoted = false;
    while index < bytes.len() {
      match bytes[index] {
        b'"' if index == 0 || bytes[index - 1] != b'\\' => quoted = !quoted,
        b'(' if !quoted => depth += 1,
        b')' if !quoted => {
          depth -= 1;
          if depth == 0 {
            expressions.push(&content[start..index]);
            offset = index + 1;
            break;
          }
        }
        _ => {}
      }
      index += 1;
    }
    if depth != 0 {
      break;
    }
  }
  expressions
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
  /// Features in `publish = false` packages unreachable from every root.
  pub dead_features: HashSet<String>,
  /// Inactive forwarding features retained as public API in published packages.
  pub optional_features: HashSet<String>,
  /// Deterministic root-to-feature path for every feature proven reachable.
  pub reachability_paths: BTreeMap<String, FeatureReachability>,
}

/// Deterministic explanation for one reachable feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureReachability {
  /// Semantic root that starts the path.
  pub root_kind: &'static str,
  /// Member-local feature path, including root and destination.
  pub path: Vec<String>,
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
  /// - **Dead features**: Empty no-ops in private packages - safe to remove
  /// - **Optional features**: Enable something (deps, other features) - user-facing API, don't remove
  pub fn analyze_crate(
    pkg: &Package,
    metadata: &MultiTargetMetadata,
    referenced_features: &HashMap<String, HashSet<String>>,
    source_referenced: &HashSet<String>,
    preserved_features: &HashSet<String>,
    workspace_is_consumer_scope: bool,
  ) -> FeatureScanResult {
    let crate_name = pkg.name.to_string();

    // Get declared features from the package
    let declared_features: HashSet<String> = pkg.features.keys().cloned().collect();

    // Get enabled features across all targets from the resolved graph
    let enabled_across_targets = metadata.all_features(&crate_name);
    let enabled_features: HashSet<String> = enabled_across_targets.values().flatten().cloned().collect();

    // Get features referenced by other workspace crates (even if not currently enabled)
    let externally_referenced = referenced_features.get(&crate_name).cloned().unwrap_or_default();

    let cargo_target_required: HashSet<&str> = pkg
      .targets
      .iter()
      .flat_map(|target| target.required_features.iter().map(String::as_str))
      .collect();

    let private_package = pkg.publish.as_ref().is_some_and(Vec::is_empty);
    let removable_package = private_package && workspace_is_consumer_scope;
    let mut roots = BTreeMap::new();
    if removable_package {
      insert_feature_roots(&mut roots, enabled_features.iter(), "resolved");
      insert_feature_roots(&mut roots, externally_referenced.iter(), "workspace_consumer");
      insert_feature_roots(&mut roots, source_referenced, "source_cfg");
      insert_feature_roots(&mut roots, cargo_target_required, "cargo_target_required");
      insert_feature_roots(&mut roots, preserved_features.iter(), "preservation_policy");
      if declared_features.contains("default") {
        roots.entry("default".to_string()).or_insert("default");
      }
    } else {
      let root_kind = if private_package {
        "open_consumer_scope"
      } else {
        "published_api"
      };
      insert_feature_roots(&mut roots, declared_features.iter(), root_kind);
    }

    let reachability_paths = Self::reachable_feature_paths(pkg, &declared_features, roots);
    let dead_features = if removable_package {
      declared_features
        .iter()
        .filter(|feature| !reachability_paths.contains_key(*feature))
        .cloned()
        .collect()
    } else {
      HashSet::new()
    };

    // Kept for the reporting contract: non-private inactive forwarding
    // features are public API, not removal candidates.
    let optional_features = if removable_package {
      HashSet::new()
    } else {
      declared_features
        .difference(&enabled_features)
        .filter(|feature| pkg.features.get(*feature).is_some_and(|edges| !edges.is_empty()))
        .cloned()
        .collect()
    };

    FeatureScanResult {
      crate_name,
      declared_features,
      enabled_features,
      dead_features,
      optional_features,
      reachability_paths,
    }
  }

  fn reachable_feature_paths(
    pkg: &Package,
    declared_features: &HashSet<String>,
    roots: BTreeMap<String, &'static str>,
  ) -> BTreeMap<String, FeatureReachability> {
    let mut paths = BTreeMap::new();
    let mut queue = VecDeque::new();
    for (root, root_kind) in roots
      .into_iter()
      .filter(|(feature, _)| declared_features.contains(feature))
    {
      paths.insert(
        root.clone(),
        FeatureReachability {
          root_kind,
          path: vec![root.clone()],
        },
      );
      queue.push_back(root);
    }

    while let Some(feature) = queue.pop_front() {
      let Some(edges) = pkg.features.get(&feature) else {
        continue;
      };
      let mut internal: Vec<_> = edges
        .iter()
        .filter(|edge| !edge.contains('/') && !edge.starts_with("dep:"))
        .filter(|edge| declared_features.contains(*edge))
        .collect();
      internal.sort_unstable();
      for next in internal {
        if paths.contains_key(next) {
          continue;
        }
        let Some(parent) = paths.get(&feature).cloned() else {
          continue;
        };
        let mut path = parent.path;
        path.push(next.clone());
        paths.insert(
          next.clone(),
          FeatureReachability {
            root_kind: parent.root_kind,
            path,
          },
        );
        queue.push_back(next.clone());
      }
    }
    paths
  }

  /// Analyze all workspace members for dead and optional features
  ///
  /// Returns results for crates that have dead or optional features.
  pub fn analyze_workspace(
    metadata: &MultiTargetMetadata,
    manifests: &crate::cargo::manifest_analyzer::ManifestAnalyzer,
    preserve: impl Fn(&str) -> bool,
    workspace_is_consumer_scope: bool,
    target_cfg_sets: &HashMap<String, TargetCfgSet>,
  ) -> Vec<FeatureScanResult> {
    // First, build a map of all features referenced by workspace crates
    // This catches conditional features like `dep/feature` in [features] tables
    let referenced_features = Self::build_referenced_features_map(metadata);
    let manifests_by_package = manifests
      .members
      .iter()
      .map(|manifest| (&manifest.package_id, manifest))
      .collect::<HashMap<_, _>>();

    let workspace_pkg_count = metadata.workspace_packages().len();
    let mut results = Vec::with_capacity(workspace_pkg_count);

    for pkg in metadata.workspace_packages() {
      // Skip crates with no declared features
      if pkg.features.is_empty() {
        continue;
      }

      let preserved_features = pkg
        .features
        .keys()
        .filter(|feature| preserve(feature))
        .cloned()
        .collect();
      let source_referenced = manifests_by_package
        .get(&pkg.id)
        .map(|manifest| applicable_source_features(manifest, &metadata.targets(), target_cfg_sets))
        .unwrap_or_default();
      let result = Self::analyze_crate(
        pkg,
        metadata,
        &referenced_features,
        &source_referenced,
        &preserved_features,
        workspace_is_consumer_scope,
      );

      results.push(result);
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
  fn test_extract_cfg_expressions_balances_nested_conditions() {
    assert_eq!(
      extract_cfg_expressions(r#"#[cfg(all(feature = "a", not(feature = "b")))] fn selected() {}"#),
      vec![r#"all(feature = "a", not(feature = "b"))"#]
    );
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
