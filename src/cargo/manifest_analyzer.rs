//! Unified manifest analysis for feature classification
//!
//! This module handles ALL manifest parsing and analysis

use crate::error::{RailResult, ResultExt};
use cargo_metadata::{DependencyKind as MetadataDepKind, PackageId};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml_edit::{DocumentMut, Item, Value};

// Core Types

/// Unique identifier for a dep
///
/// Uses `Arc<str>` for the name fields to avoid expensive clones in hot loops.
/// Cloning a DepKey is cheap (just Arc refcount bumps).
#[derive(Debug, Clone)]
pub struct DepKey {
  /// Dependency name (package name)
  pub name: Arc<str>,
  /// Whether it's renamed (package = "actual-name")
  pub renamed_from: Option<Arc<str>>,
}

impl DepKey {
  /// Creates a new dependency key with the given name
  pub fn new(name: impl Into<Arc<str>>) -> Self {
    Self {
      name: name.into(),
      renamed_from: None,
    }
  }

  /// Check if this is a renamed dependency
  pub fn is_renamed(&self) -> bool {
    self.renamed_from.is_some()
  }

  /// Get the alias (key used in Cargo.toml) - either renamed_from or name
  pub fn alias(&self) -> &str {
    self.renamed_from.as_deref().unwrap_or(&self.name)
  }
}

// Compare by both name AND renamed_from to distinguish:
// - getrandom (direct dependency)
// - old_getrandom (renamed from getrandom, i.e., package = "getrandom")
// These are different dependencies that should NOT have their features merged.
impl PartialEq for DepKey {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name && self.renamed_from == other.renamed_from
  }
}

impl Eq for DepKey {}

impl std::hash::Hash for DepKey {
  fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
    self.name.hash(state);
    self.renamed_from.hash(state);
  }
}

impl PartialOrd for DepKey {
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for DepKey {
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    match self.name.cmp(&other.name) {
      std::cmp::Ordering::Equal => self.renamed_from.cmp(&other.renamed_from),
      other => other,
    }
  }
}

/// How a dependency is used at a specific site
///
/// Uses `Arc<str>` for frequently-cloned identifier fields to avoid allocation overhead.
#[derive(Debug, Clone)]
pub struct DepUsage {
  /// Features explicitly listed in the dependency entry
  pub unconditional_features: BTreeSet<String>,
  /// Features referenced in `[features]` table (dep/feat syntax)
  pub conditional_features: BTreeSet<String>,
  /// Whether default features are enabled
  pub default_features: bool,
  /// Dependency kind (Normal, Dev, Build)
  pub kind: DepKind,
  /// Target constraint (e.g., "cfg(unix)")
  pub target: Option<String>,
  /// The crate that uses this dependency (Arc for cheap clones)
  pub used_by: Arc<str>,
  /// Whether the dependency is optional
  pub optional: bool,
  /// Path for path dependencies (if specified)
  pub path: Option<String>,
  /// Declared version requirement from Cargo.toml (e.g., "1.0", "^2.0", ">=1.5,<2")
  /// This is the version declared by the user, NOT the resolved version from cargo metadata
  pub declared_version: Option<String>,
  /// Path to the manifest that declared this dependency (for path normalization)
  pub manifest_path: Option<PathBuf>,
  /// The key name used in Cargo.toml (alias if renamed, otherwise package name)
  /// Used when generating manifest edits to target the correct dependency entry (Arc for cheap clones)
  pub cargo_toml_key: Arc<str>,
  /// Whether this optional dep is referenced in the `[features]` table
  /// True if the dep appears as: `dep:name`, `name` (for optional deps), or `name/feat`
  /// Used to distinguish truly unused optional deps from feature-gated ones
  pub referenced_in_features: bool,
}

/// Parsed dependency table info (used internally during manifest parsing)
#[derive(Debug, Default)]
struct ParsedDepTable {
  renamed_from: Option<String>,
  actual_name: Option<String>,
  unconditional_features: BTreeSet<String>,
  default_features: bool,
  optional: bool,
  path: Option<String>,
  declared_version: Option<String>,
}

/// Type of dependency (normal, dev, or build)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DepKind {
  /// Normal runtime dependency
  Normal,
  /// Development-only dependency
  Dev,
  /// Build-time dependency
  Build,
}

impl DepKind {
  /// Stable machine-readable dependency domain.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Normal => "normal",
      Self::Dev => "development",
      Self::Build => "build",
    }
  }
}

impl From<MetadataDepKind> for DepKind {
  fn from(kind: MetadataDepKind) -> Self {
    match kind {
      MetadataDepKind::Normal => DepKind::Normal,
      MetadataDepKind::Development => DepKind::Dev,
      MetadataDepKind::Build => DepKind::Build,
      _ => DepKind::Normal,
    }
  }
}

/// Parsed manifest for a workspace member
#[derive(Debug)]
pub struct ParsedManifest {
  /// Opaque Cargo package identity.
  pub package_id: PackageId,
  /// Path to the Cargo.toml
  pub path: PathBuf,
  /// Package name
  pub package_name: String,
  /// Feature names declared by this package.
  pub declared_features: BTreeSet<String>,
  /// Feature selections required by Cargo targets such as examples and binaries.
  pub required_feature_selections: BTreeSet<Vec<String>>,
  /// All dependencies with their usages (Vec because same dep can appear in multiple sections)
  pub dependencies: HashMap<DepKey, Vec<DepUsage>>,
}

// Parse Context

/// Context for parsing dependency sections within a single manifest.
/// Bundles common parameters to reduce function argument count.
struct ParseContext<'a> {
  /// Package name of the crate being parsed
  package_name: &'a str,
  /// Path to the Cargo.toml being parsed
  manifest_path: &'a Path,
  /// Output map to collect parsed dependencies (Vec allows same dep in multiple sections)
  dependencies: &'a mut HashMap<DepKey, Vec<DepUsage>>,
}

// Main Analyzer

/// Analyzes all workspace manifests for dependency usage patterns
pub struct ManifestAnalyzer {
  /// All parsed manifests
  pub members: Vec<ParsedManifest>,
  /// Dependency usage index: dep -> list of usage sites
  usage_index: HashMap<DepKey, Vec<DepUsage>>,
  /// Pre-computed usage counts for O(1) lookup
  usage_counts: HashMap<DepKey, usize>,
  /// Package name index for O(1) lookup of dep keys by package name
  /// Maps package_name -> list of DepKeys that refer to that package
  package_index: HashMap<Arc<str>, Vec<DepKey>>,
}

impl ManifestAnalyzer {
  fn unconditional_usages(&self, dep: &DepKey) -> Vec<&DepUsage> {
    self
      .usage_index
      .get(dep)
      .map(|usages| usages.iter().filter(|u| u.target.is_none()).collect())
      .unwrap_or_default()
  }

  /// Parse all workspace member manifests (in parallel)
  pub fn parse_workspace(_workspace_root: &Path, members: &[&cargo_metadata::Package]) -> RailResult<Self> {
    // Parse manifests in parallel for 50-70% speedup
    let results: Vec<RailResult<ParsedManifest>> = members
      .par_iter()
      .map(|pkg| {
        let manifest_path = pkg.manifest_path.as_std_path();
        let required_feature_selections = pkg
          .targets
          .iter()
          .filter(|target| !target.required_features.is_empty())
          .map(|target| {
            let mut features = target.required_features.clone();
            features.sort_unstable();
            features
          })
          .collect();
        Self::parse_single_manifest(manifest_path, &pkg.id, &pkg.name, required_feature_selections)
      })
      .collect();

    // Collect results, propagating any errors
    let mut parsed_members = Vec::with_capacity(results.len());
    for result in results {
      parsed_members.push(result?);
    }

    // Build usage index
    // DepKey now includes renamed_from in equality, so:
    // - `getrandom` (direct) and `old_getrandom` (package = "getrandom") are separate keys
    // - This prevents feature confusion between renamed and non-renamed deps
    let mut usage_index: HashMap<DepKey, Vec<DepUsage>> = HashMap::new();

    for member in &parsed_members {
      for (dep_key, usages) in &member.dependencies {
        usage_index
          .entry(dep_key.clone())
          .or_default()
          .extend(usages.iter().cloned());
      }
    }

    // Pre-compute usage counts for O(1) lookup (20-30% speedup)
    let usage_counts: HashMap<DepKey, usize> = usage_index
      .iter()
      .map(|(key, usages)| {
        let unique_users: HashSet<_> = usages.iter().map(|u| &u.used_by).collect();
        (key.clone(), unique_users.len())
      })
      .collect();

    // Build package name index for O(1) lookup of dep keys by package name
    let mut package_index: HashMap<Arc<str>, Vec<DepKey>> = HashMap::new();
    for dep_key in usage_index.keys() {
      package_index
        .entry(Arc::clone(&dep_key.name))
        .or_default()
        .push(dep_key.clone());
    }

    Ok(Self {
      members: parsed_members,
      usage_index,
      usage_counts,
      package_index,
    })
  }

  /// Parse a single manifest file
  fn parse_single_manifest(
    manifest_path: &Path,
    package_id: &PackageId,
    package_name: &str,
    required_feature_selections: BTreeSet<Vec<String>>,
  ) -> RailResult<ParsedManifest> {
    let content =
      std::fs::read_to_string(manifest_path).with_context(|| format!("Failed to read {}", manifest_path.display()))?;

    let doc: DocumentMut = content
      .parse()
      .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    let mut dependencies = HashMap::new();
    let mut ctx = ParseContext {
      package_name,
      manifest_path,
      dependencies: &mut dependencies,
    };

    // Parse [dependencies]
    Self::parse_dep_section(&mut ctx, doc.as_item(), "dependencies", DepKind::Normal, None)?;

    // Parse [dev-dependencies]
    Self::parse_dep_section(&mut ctx, doc.as_item(), "dev-dependencies", DepKind::Dev, None)?;

    // Parse [build-dependencies]
    Self::parse_dep_section(&mut ctx, doc.as_item(), "build-dependencies", DepKind::Build, None)?;

    // Parse [target.'cfg(...)'.dependencies]
    if let Some(target_table) = doc.get("target").and_then(|t| t.as_table()) {
      for (target_cfg, target_value) in target_table {
        if let Some(target_deps) = target_value.as_table() {
          // Parse each dependency section under this target
          for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if target_deps.contains_key(section) {
              let kind = match section {
                "dependencies" => DepKind::Normal,
                "dev-dependencies" => DepKind::Dev,
                "build-dependencies" => DepKind::Build,
                _ => continue,
              };

              Self::parse_dep_section(&mut ctx, target_value, section, kind, Some(target_cfg))?;
            }
          }
        }
      }
    }

    // Parse [features] table to find conditional feature references and dep activations
    // This detects three patterns that reference dependencies:
    // 1. "dep:name" - explicit dep activation (Rust 2021+)
    // 2. "name" - implicit optional dep activation (when name matches an optional dep)
    // 3. "name/feat" - dep with specific feature enabled
    let mut declared_features = BTreeSet::new();
    if let Some(features_table) = doc.get("features").and_then(|f| f.as_table()) {
      declared_features.extend(features_table.iter().map(|(name, _)| name.to_string()));
      for (_feature_name, feature_value) in features_table {
        if let Some(feature_list) = feature_value.as_array() {
          for item in feature_list {
            if let Some(s) = item.as_str() {
              // Pattern 1: "dep:name" - explicit dep reference (Rust 2021+)
              if let Some(dep_name) = s.strip_prefix("dep:") {
                let dep_key = DepKey::new(dep_name);
                if let Some(usages) = dependencies.get_mut(&dep_key) {
                  for usage in usages {
                    usage.referenced_in_features = true;
                  }
                }
              }
              // Pattern 2: "name/feat" - dep with feature
              else if let Some((dep, feat)) = s.split_once('/') {
                // Strip optional "dep:" prefix from the dep part
                let dep_name = dep.strip_prefix("dep:").unwrap_or(dep);
                let dep_key = DepKey::new(dep_name);
                if let Some(usages) = dependencies.get_mut(&dep_key) {
                  for usage in usages {
                    usage.conditional_features.insert(feat.to_string());
                    usage.referenced_in_features = true;
                  }
                }
              }
              // Pattern 3: bare "name" - check if it matches an optional dep
              else {
                let dep_key = DepKey::new(s);
                if let Some(usages) = dependencies.get_mut(&dep_key) {
                  for usage in usages {
                    // Only count as referenced if the dep is optional
                    // (non-optional deps with same name as features are already resolved)
                    if usage.optional {
                      usage.referenced_in_features = true;
                    }
                  }
                }
              }
            }
          }
        }
      }
    }

    Ok(ParsedManifest {
      package_id: package_id.clone(),
      path: manifest_path.to_path_buf(),
      package_name: package_name.to_string(),
      declared_features,
      required_feature_selections,
      dependencies,
    })
  }

  /// Parse a dependency section
  fn parse_dep_section(
    ctx: &mut ParseContext<'_>,
    parent: &Item,
    section: &str,
    kind: DepKind,
    target: Option<&str>,
  ) -> RailResult<()> {
    let Some(deps_table) = parent.get(section).and_then(|d| d.as_table_like()) else {
      return Ok(()); // Section doesn't exist
    };

    for (dep_name, dep_value) in deps_table.iter() {
      // Parse the dependency value
      let parsed = match dep_value {
        Item::Value(Value::String(version_str)) => {
          // Simple version string: dep = "1.0"
          Some(ParsedDepTable {
            declared_version: Some(version_str.value().to_string()),
            default_features: true,
            ..Default::default()
          })
        }
        Item::Value(Value::InlineTable(inline_table)) => Self::parse_dep_table(inline_table, dep_name),
        Item::Table(table) => Self::parse_dep_table(table, dep_name),
        _ => None,
      };

      if let Some(p) = parsed {
        let dep_key = if let Some(actual) = p.actual_name {
          DepKey {
            name: actual.into(),
            renamed_from: p.renamed_from.map(Into::into),
          }
        } else {
          DepKey::new(dep_name)
        };

        // cargo_toml_key is always the key used in Cargo.toml (dep_name),
        // which is the alias if renamed, or the package name otherwise
        let usage = DepUsage {
          unconditional_features: p.unconditional_features,
          conditional_features: BTreeSet::new(), // Filled in later by features parsing
          default_features: p.default_features,
          kind,
          target: target.map(String::from),
          used_by: Arc::from(ctx.package_name),
          optional: p.optional,
          path: p.path,
          declared_version: p.declared_version,
          manifest_path: Some(ctx.manifest_path.to_path_buf()),
          cargo_toml_key: Arc::from(dep_name),
          referenced_in_features: false, // Filled in later by features parsing
        };

        ctx.dependencies.entry(dep_key).or_default().push(usage);
      }
    }

    Ok(())
  }

  /// Parse a dependency table (either InlineTable or Table)
  /// Returns None if the dependency should be skipped (uses workspace inheritance)
  fn parse_dep_table<T: toml_edit::TableLike>(table: &T, dep_name: &str) -> Option<ParsedDepTable> {
    // Skip dependencies that already use workspace inheritance
    if table.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
      return None;
    }

    let mut parsed = ParsedDepTable {
      default_features: true, // Cargo default
      ..Default::default()
    };

    // Check for renamed package (only when package name differs from dependency key)
    if let Some(pkg) = table.get("package").and_then(|v| v.as_str()) {
      // Only flag as renamed when there's actual renaming, not redundant package fields
      if pkg != dep_name {
        parsed.renamed_from = Some(dep_name.to_string());
        parsed.actual_name = Some(pkg.to_string());
      }
    }

    // Parse version - this is the DECLARED version from the manifest
    parsed.declared_version = table.get("version").and_then(|v| v.as_str()).map(String::from);

    // Parse path for path dependencies
    parsed.path = table.get("path").and_then(|v| v.as_str()).map(String::from);

    // Parse features
    if let Some(features) = table.get("features").and_then(|f| f.as_array()) {
      for feat in features {
        if let Some(s) = feat.as_str() {
          parsed.unconditional_features.insert(s.to_string());
        }
      }
    }

    // Parse default-features
    if let Some(df) = table.get("default-features").and_then(|v| v.as_bool()) {
      parsed.default_features = df;
    }

    // Parse optional
    if let Some(opt) = table.get("optional").and_then(|v| v.as_bool()) {
      parsed.optional = opt;
    }

    Some(parsed)
  }

  /// Compute the union of all features used across all usage sites
  ///
  /// Used when mixed default-features are detected or intersection is empty.
  /// Includes ALL dep kinds (Normal, Dev, Build) per design doc requirements.
  ///
  /// **IMPORTANT**: Only includes features from UNCONDITIONAL usages (no target constraint).
  /// Features from target-specific usages (e.g., `[target.'cfg(linux)'.dependencies]`)
  /// are excluded because they may not be valid on all platforms and should stay local.
  pub fn compute_union(&self, dep: &DepKey) -> BTreeSet<String> {
    let Some(usages) = self.usage_index.get(dep) else {
      return BTreeSet::new();
    };

    // Include ALL dep kinds - workspace deps serve all usage contexts
    if usages.is_empty() {
      return BTreeSet::new();
    }

    // Union features from UNCONDITIONAL usages only
    // Target-specific features stay local to avoid platform incompatibilities
    let mut union = BTreeSet::new();
    for usage in usages {
      if usage.target.is_none() {
        union.extend(usage.unconditional_features.iter().cloned());
      }
    }

    union
  }

  /// Compute features that are ONLY declared with target constraints
  ///
  /// These features should stay local (in member Cargo.toml) because they may
  /// have platform-specific requirements (like cfg flags or OS restrictions).
  /// Produces features that appear only in target-constrained usages.
  pub fn compute_target_local_features(&self, dep: &DepKey) -> BTreeSet<String> {
    let Some(usages) = self.usage_index.get(dep) else {
      return BTreeSet::new();
    };

    // Collect features from target-constrained usages
    let mut target_features = BTreeSet::new();
    for usage in usages {
      if usage.target.is_some() {
        target_features.extend(usage.unconditional_features.iter().cloned());
      }
    }

    // Subtract features that also appear unconditionally
    let unconditional = self.compute_union(dep);
    target_features.difference(&unconditional).cloned().collect()
  }

  /// Compute the intersection of features used by all packages that depend on this dependency
  ///
  /// This is the CORE of the minimal feature approach:
  /// Only features that are ALWAYS enabled go into [workspace.dependencies].
  /// Members that need more can add local features.
  /// Includes ALL dep kinds (Normal, Dev, Build) per design doc requirements.
  ///
  /// **IMPORTANT**: Only considers UNCONDITIONAL usages (no target constraint).
  /// Target-specific usages are excluded because their features may have
  /// platform-specific requirements.
  pub fn compute_intersection(&self, dep: &DepKey) -> BTreeSet<String> {
    let unconditional_usages = self.unconditional_usages(dep);

    // Include ALL dep kinds - workspace deps serve all usage contexts
    if unconditional_usages.len() < 2 {
      return BTreeSet::new(); // Not enough unconditional uses to unify
    }

    // Start with the first unconditional usage's features
    let mut intersection = unconditional_usages[0].unconditional_features.clone();

    // Intersect with all other unconditional usages
    for usage in &unconditional_usages[1..] {
      intersection = intersection
        .intersection(&usage.unconditional_features)
        .cloned()
        .collect();
    }

    intersection
  }

  /// Get all usage sites for a dependency
  pub fn get_usage_sites(&self, dep: &DepKey) -> Vec<&DepUsage> {
    self
      .usage_index
      .get(dep)
      .map(|v| v.iter().collect())
      .unwrap_or_default()
  }

  /// Get all dependencies used in the workspace
  pub fn all_dependencies(&self) -> Vec<&DepKey> {
    self.usage_index.keys().collect()
  }

  /// Check if a dependency has mixed default-features settings
  ///
  /// Includes every dependency kind and target domain because workspace-level
  /// defaults cannot be disabled by a narrower inherited declaration.
  pub fn has_mixed_defaults(&self, dep: &DepKey) -> bool {
    let Some(usages) = self.usage_index.get(dep) else {
      return false;
    };

    if usages.len() < 2 {
      return false;
    }

    let first_default = usages[0].default_features;
    !usages.iter().all(|usage| usage.default_features == first_default)
  }

  /// Determine the default-features policy for a dependency
  ///
  /// Includes every dependency kind and target domain. If any declaration
  /// disables defaults, the workspace baseline must disable them and opt the
  /// other declarations back in through the explicit `default` feature.
  pub fn default_features_policy(&self, dep: &DepKey) -> Option<bool> {
    let usages = self.usage_index.get(dep)?;
    if usages.is_empty() {
      return None;
    }

    if usages.iter().any(|usage| !usage.default_features) {
      Some(false)
    } else {
      Some(true)
    }
  }

  /// Count how many crates use a dependency (O(1) lookup from pre-computed cache)
  pub fn usage_count(&self, dep: &DepKey) -> usize {
    self.usage_counts.get(dep).copied().unwrap_or(0)
  }

  /// Count how many crates use a package (aggregated across renamed and non-renamed deps)
  ///
  /// When include_renamed = true, count all usages of the package
  /// Uses package_index for O(1) key lookup instead of O(n) scan.
  pub fn package_usage_count(&self, package_name: &str) -> usize {
    let Some(dep_keys) = self.package_index.get(package_name) else {
      return 0;
    };

    let mut unique_users: HashSet<&str> = HashSet::new();
    for dep_key in dep_keys {
      if let Some(usages) = self.usage_index.get(dep_key) {
        for usage in usages {
          unique_users.insert(&usage.used_by);
        }
      }
    }

    unique_users.len()
  }

  /// Get all dep keys that refer to a specific package (including renamed)
  ///
  /// Used to find all renamed variants of a package
  /// Uses package_index for O(1) lookup instead of O(n) scan.
  pub fn dep_keys_for_package(&self, package_name: &str) -> Vec<&DepKey> {
    self
      .package_index
      .get(package_name)
      .map(|keys| keys.iter().collect())
      .unwrap_or_default()
  }

  /// Get aggregated usage sites for a package (all renamed and non-renamed usages)
  ///
  /// Used when include_renamed = true to merge features across all usages
  /// Uses package_index for O(1) key lookup instead of O(n) scan.
  pub fn get_package_usage_sites(&self, package_name: &str) -> Vec<&DepUsage> {
    let Some(dep_keys) = self.package_index.get(package_name) else {
      return Vec::new();
    };

    let mut all_usages = Vec::new();
    for dep_key in dep_keys {
      if let Some(usages) = self.usage_index.get(dep_key) {
        all_usages.extend(usages.iter());
      }
    }

    all_usages
  }

  /// Compute union of features across all usages of a package (including renamed)
  ///
  /// When include_renamed = true, aggregate features from all variants
  pub fn compute_package_union(&self, package_name: &str) -> BTreeSet<String> {
    let mut union = BTreeSet::new();

    for usage in self.get_package_usage_sites(package_name) {
      if usage.target.is_none() {
        union.extend(usage.unconditional_features.iter().cloned());
      }
    }

    union
  }

  /// Check if a package has mixed default-features across all usages (including renamed)
  ///
  /// When include_renamed = true, check across all variants
  pub fn package_has_mixed_defaults(&self, package_name: &str) -> bool {
    let usages = self.get_package_usage_sites(package_name);

    if usages.len() < 2 {
      return false;
    }

    let first_default = usages[0].default_features;
    !usages.iter().all(|u| u.default_features == first_default)
  }

  /// Get default-features policy across all usages of a package (including renamed)
  ///
  /// When include_renamed = true, use conservative policy across all variants
  pub fn package_default_features_policy(&self, package_name: &str) -> Option<bool> {
    let usages = self.get_package_usage_sites(package_name);

    if usages.is_empty() {
      return None;
    }

    if usages.iter().any(|usage| !usage.default_features) {
      Some(false)
    } else {
      Some(true)
    }
  }

  /// Get unique package names from all dependencies
  ///
  /// Used to iterate by package rather than by dep key
  pub fn unique_packages(&self) -> HashSet<Arc<str>> {
    self.usage_index.keys().map(|k| Arc::clone(&k.name)).collect()
  }
}

// Workspace Dependencies Parser

/// Information about an existing workspace dependency
#[derive(Debug, Clone)]
pub struct ExistingWorkspaceDep {
  /// Dependency name
  pub name: String,
  /// Version requirement (if specified)
  pub version: Option<String>,
  /// Features enabled
  pub features: Vec<String>,
  /// Whether default features are enabled
  pub default_features: bool,
  /// Path for path dependencies
  pub path: Option<String>,
}

/// Parse existing [workspace.dependencies] from the workspace root Cargo.toml
///
/// Produces a map of dependency name to current workspace configuration.
/// This is used to detect deps that already exist in workspace.dependencies
/// so we don't add duplicates.
pub fn parse_existing_workspace_deps(workspace_root: &Path) -> RailResult<FxHashMap<String, ExistingWorkspaceDep>> {
  let workspace_toml = workspace_root.join("Cargo.toml");
  let content = match std::fs::read_to_string(&workspace_toml) {
    Ok(c) => c,
    Err(_) => return Ok(FxHashMap::default()), // No workspace Cargo.toml
  };

  let doc: DocumentMut = content
    .parse()
    .with_context(|| format!("Failed to parse {}", workspace_toml.display()))?;

  let mut existing = FxHashMap::default();

  // Check for [workspace.dependencies] section
  let Some(workspace) = doc.get("workspace").and_then(|w| w.as_table()) else {
    return Ok(existing); // No [workspace] section
  };

  let Some(deps) = workspace.get("dependencies").and_then(|d| d.as_table_like()) else {
    return Ok(existing); // No [workspace.dependencies] section
  };

  for (name, value) in deps.iter() {
    let dep = parse_workspace_dep_entry(name, value);
    existing.insert(name.to_string(), dep);
  }

  Ok(existing)
}

/// Parse a single workspace dependency entry
fn parse_workspace_dep_entry(name: &str, value: &Item) -> ExistingWorkspaceDep {
  let mut dep = ExistingWorkspaceDep {
    name: name.to_string(),
    version: None,
    features: Vec::new(),
    default_features: true,
    path: None,
  };

  // Simple version string: dep = "1.0"
  if let Some(version) = value.as_str() {
    dep.version = Some(version.to_string());
    return dep;
  }

  // Inline table: dep = { version = "1.0", features = [...] }
  if let Some(table) = value.as_inline_table() {
    if let Some(v) = table.get("version").and_then(|v| v.as_str()) {
      dep.version = Some(v.to_string());
    }
    if let Some(p) = table.get("path").and_then(|v| v.as_str()) {
      dep.path = Some(p.to_string());
    }
    if let Some(df) = table.get("default-features").and_then(|v| v.as_bool()) {
      dep.default_features = df;
    }
    if let Some(features) = table.get("features").and_then(|f| f.as_array()) {
      dep.features = features.iter().filter_map(|v| v.as_str()).map(String::from).collect();
    }
    return dep;
  }

  // Full table: [workspace.dependencies.dep]
  if let Some(table) = value.as_table() {
    if let Some(v) = table.get("version").and_then(|i| i.as_value()).and_then(|v| v.as_str()) {
      dep.version = Some(v.to_string());
    }
    if let Some(p) = table.get("path").and_then(|i| i.as_value()).and_then(|v| v.as_str()) {
      dep.path = Some(p.to_string());
    }
    if let Some(df) = table
      .get("default-features")
      .and_then(|i| i.as_value())
      .and_then(|v| v.as_bool())
    {
      dep.default_features = df;
    }
    if let Some(features) = table
      .get("features")
      .and_then(|i| i.as_value())
      .and_then(|v| v.as_array())
    {
      dep.features = features.iter().filter_map(|v| v.as_str()).map(String::from).collect();
    }
  }

  dep
}

// Unit Tests

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;
  use tempfile::TempDir;

  fn create_test_workspace(workspace_toml_content: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let mut file = std::fs::File::create(dir.path().join("Cargo.toml")).unwrap();
    file.write_all(workspace_toml_content.as_bytes()).unwrap();
    dir
  }

  #[test]
  fn test_parse_existing_workspace_deps_empty() {
    let dir = create_test_workspace(
      r#"
[workspace]
members = ["crate-a"]
"#,
    );

    let result = parse_existing_workspace_deps(dir.path()).unwrap();
    assert!(
      result.is_empty(),
      "Should return empty map when no workspace.dependencies"
    );
  }

  #[test]
  fn test_parse_existing_workspace_deps_simple_version() {
    let dir = create_test_workspace(
      r#"
[workspace]
members = ["crate-a"]

[workspace.dependencies]
serde = "1.0"
anyhow = "1.0.50"
"#,
    );

    let result = parse_existing_workspace_deps(dir.path()).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result["serde"].version.as_ref().unwrap(), "1.0");
    assert_eq!(result["anyhow"].version.as_ref().unwrap(), "1.0.50");
    assert!(result["serde"].features.is_empty());
    assert!(result["serde"].default_features);
  }

  #[test]
  fn test_parse_existing_workspace_deps_inline_table() {
    let dir = create_test_workspace(
      r#"
[workspace]
members = ["crate-a"]

[workspace.dependencies]
serde = { version = "1.0", features = ["derive", "rc"], default-features = false }
tokio = { version = "1.0", features = ["full"] }
"#,
    );

    let result = parse_existing_workspace_deps(dir.path()).unwrap();
    assert_eq!(result.len(), 2);

    let serde = &result["serde"];
    assert_eq!(serde.version.as_ref().unwrap(), "1.0");
    assert_eq!(serde.features, vec!["derive", "rc"]);
    assert!(!serde.default_features);

    let tokio = &result["tokio"];
    assert_eq!(tokio.version.as_ref().unwrap(), "1.0");
    assert_eq!(tokio.features, vec!["full"]);
    assert!(tokio.default_features);
  }

  #[test]
  fn test_parse_existing_workspace_deps_with_path() {
    let dir = create_test_workspace(
      r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.dependencies]
crate-a = { path = "crate-a", version = "0.1.0" }
external = "1.0"
"#,
    );

    let result = parse_existing_workspace_deps(dir.path()).unwrap();
    assert_eq!(result.len(), 2);

    let crate_a = &result["crate-a"];
    assert_eq!(crate_a.path.as_ref().unwrap(), "crate-a");
    assert_eq!(crate_a.version.as_ref().unwrap(), "0.1.0");

    let external = &result["external"];
    assert!(external.path.is_none());
  }
}
