//! Parse workspace manifests and classify dependency feature usage.

use crate::error::{RailError, RailResult, ResultExt};
use crate::workspace::WorkspaceSnapshot;
use cargo_metadata::{DependencyKind as MetadataDepKind, PackageId};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml_edit::{DocumentMut, Item, Value};

// Core Types

/// Cargo package fields that may inherit from `[workspace.package]` and are
/// analyzed by unify. `rust-version` remains owned by the MSRV policy.
const PACKAGE_INHERITANCE_FIELDS: [&str; 15] = [
  "authors",
  "categories",
  "description",
  "documentation",
  "edition",
  "exclude",
  "homepage",
  "include",
  "keywords",
  "license",
  "license-file",
  "publish",
  "readme",
  "repository",
  "version",
];

/// Captured semantic value for one Cargo-inheritable package field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PackageFieldValue {
  String(String),
  Boolean(bool),
  Strings(Vec<String>),
}

/// Captured declaration state for one member package field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PackageFieldDeclaration {
  Inherited,
  Explicit(PackageFieldValue),
}

/// One deterministic package-inheritance analysis record.
#[derive(Debug, Clone)]
pub(crate) struct PackageInheritanceAnalysis {
  pub(crate) field: String,
  pub(crate) inherited: Vec<String>,
  pub(crate) planned: Vec<String>,
  pub(crate) local_overrides: Vec<String>,
  pub(crate) missing: Vec<String>,
  pub(crate) retained_equivalent: Vec<String>,
  pub(crate) retention_reason: Option<&'static str>,
}

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
  /// Features written on this exact member declaration, excluding inherited workspace features.
  pub local_features: BTreeSet<String>,
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
  /// Whether this declaration already uses `{ workspace = true }`.
  pub workspace_inherited: bool,
}

/// Parsed dependency table info (used internally during manifest parsing)
#[derive(Debug, Default)]
struct ParsedDepTable {
  renamed_from: Option<String>,
  actual_name: Option<String>,
  unconditional_features: BTreeSet<String>,
  local_features: BTreeSet<String>,
  default_features: bool,
  optional: bool,
  path: Option<String>,
  declared_version: Option<String>,
  workspace_inherited: bool,
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
  /// Feature selections derived from source conditions in the captured workspace.
  pub(crate) condition_feature_selections: BTreeSet<Vec<String>>,
  /// Feature names referenced by source conditions in the captured workspace.
  pub(crate) source_cfg_features: BTreeSet<String>,
  /// Parsed cfg expressions retained for captured target applicability.
  pub(crate) source_cfg_expressions: BTreeSet<String>,
  /// Captured source/README identifiers that conservatively retain dependency declarations.
  pub(crate) retention_identifiers: BTreeSet<String>,
  /// Whether `[package].rust-version` inherits the captured workspace value.
  pub(crate) inherits_workspace_msrv: bool,
  /// Captured declarations for Cargo-inheritable package fields other than MSRV.
  pub(crate) package_fields: BTreeMap<String, PackageFieldDeclaration>,
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
  workspace_dependencies: &'a FxHashMap<String, ExistingWorkspaceDep>,
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
  /// Captured `[workspace.dependencies]` entries keyed by their Cargo.toml alias.
  workspace_dependencies: FxHashMap<String, ExistingWorkspaceDep>,
  /// Captured `[workspace.package]` values, excluding MSRV policy.
  workspace_package_fields: BTreeMap<String, PackageFieldValue>,
}

impl ManifestAnalyzer {
  fn unconditional_usages(&self, dep: &DepKey) -> Vec<&DepUsage> {
    self
      .usage_index
      .get(dep)
      .map(|usages| usages.iter().filter(|u| u.target.is_none()).collect())
      .unwrap_or_default()
  }

  /// Parse all workspace member manifests from one authoritative snapshot.
  pub(crate) fn parse_snapshot(snapshot: &WorkspaceSnapshot, members: &[&cargo_metadata::Package]) -> RailResult<Self> {
    let workspace_manifest = snapshot.workspace_manifest()?;
    let workspace_manifest_path = snapshot.source_root().join(workspace_manifest.path().as_path());
    let (workspace_dependencies, workspace_package_fields) =
      parse_workspace_manifest_policy(&workspace_manifest_path, workspace_manifest.bytes())?;
    let workspace_dependencies = Arc::new(workspace_dependencies);
    let package_locations = snapshot
      .packages()
      .iter()
      .filter_map(|package| Some((package.id(), (package.manifest_path()?, package.package_root()?))))
      .collect::<HashMap<_, _>>();
    let manifests = snapshot
      .manifests()
      .iter()
      .map(|manifest| (manifest.path(), manifest))
      .collect::<HashMap<_, _>>();

    // Parse independent captured manifests and source conditions in parallel.
    let results: Vec<RailResult<ParsedManifest>> = members
      .par_iter()
      .map(|pkg| {
        let workspace_dependencies = Arc::clone(&workspace_dependencies);
        let (manifest_path, package_root) = package_locations.get(&pkg.id).copied().ok_or_else(|| {
          RailError::message(format!(
            "workspace package '{}' is absent from the captured local package inventory",
            pkg.id
          ))
        })?;
        let manifest = manifests.get(manifest_path).copied().ok_or_else(|| {
          RailError::message(format!(
            "workspace package '{}' manifest '{}' is absent from the workspace snapshot",
            pkg.id, manifest_path
          ))
        })?;
        let absolute_manifest_path = snapshot.source_root().join(manifest_path.as_path());
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
        let source_features = crate::cargo::feature_scanner::scan_snapshot_source_features(snapshot, package_root)?;
        Self::parse_manifest(
          &absolute_manifest_path,
          manifest.bytes(),
          &pkg.id,
          &pkg.name,
          required_feature_selections,
          source_features,
          &workspace_dependencies,
        )
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

    // Pre-compute usage counts for O(1) lookup.
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
      workspace_dependencies: Arc::unwrap_or_clone(workspace_dependencies),
      workspace_package_fields,
    })
  }

  /// Return captured workspace dependency policy keyed by manifest alias.
  pub(crate) fn workspace_dependencies(&self) -> &FxHashMap<String, ExistingWorkspaceDep> {
    &self.workspace_dependencies
  }

  /// Parse one exact captured manifest.
  fn parse_manifest(
    manifest_path: &Path,
    bytes: &[u8],
    package_id: &PackageId,
    package_name: &str,
    required_feature_selections: BTreeSet<Vec<String>>,
    source_features: crate::cargo::feature_scanner::CapturedSourceFeatures,
    workspace_dependencies: &FxHashMap<String, ExistingWorkspaceDep>,
  ) -> RailResult<ParsedManifest> {
    let content = std::str::from_utf8(bytes)
      .map_err(|_| RailError::message(format!("Failed to parse {} as UTF-8", manifest_path.display())))?;

    let doc: DocumentMut = content
      .parse()
      .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    let mut dependencies = HashMap::new();
    let inherits_workspace_msrv = doc
      .get("package")
      .and_then(|package| package.get("rust-version"))
      .and_then(Item::as_table_like)
      .and_then(|rust_version| rust_version.get("workspace"))
      .and_then(Item::as_bool)
      == Some(true);
    let package_fields = parse_member_package_fields(&doc, manifest_path)?;
    let mut ctx = ParseContext {
      package_name,
      manifest_path,
      dependencies: &mut dependencies,
      workspace_dependencies,
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

    let crate::cargo::feature_scanner::CapturedSourceFeatures {
      selections,
      referenced,
      expressions,
      retention_identifiers,
    } = source_features;
    Ok(ParsedManifest {
      package_id: package_id.clone(),
      path: manifest_path.to_path_buf(),
      package_name: package_name.to_string(),
      declared_features,
      required_feature_selections,
      condition_feature_selections: selections.into_iter().collect(),
      source_cfg_features: referenced,
      source_cfg_expressions: expressions,
      retention_identifiers,
      inherits_workspace_msrv,
      package_fields,
      dependencies,
    })
  }

  /// Compare member package declarations with the captured workspace policy.
  pub(crate) fn package_inheritance_analysis(&self) -> Vec<PackageInheritanceAnalysis> {
    self
      .workspace_package_fields
      .iter()
      .map(|(field, workspace_value)| {
        let retention_reason = package_field_retention_reason(field);
        let mut analysis = PackageInheritanceAnalysis {
          field: field.clone(),
          inherited: Vec::new(),
          planned: Vec::new(),
          local_overrides: Vec::new(),
          missing: Vec::new(),
          retained_equivalent: Vec::new(),
          retention_reason,
        };
        for member in &self.members {
          match member.package_fields.get(field) {
            Some(PackageFieldDeclaration::Inherited) => analysis.inherited.push(member.package_name.clone()),
            Some(PackageFieldDeclaration::Explicit(value)) if value == workspace_value => {
              if retention_reason.is_some() {
                analysis.retained_equivalent.push(member.package_name.clone());
              } else {
                analysis.planned.push(member.package_name.clone());
              }
            }
            Some(PackageFieldDeclaration::Explicit(_)) => analysis.local_overrides.push(member.package_name.clone()),
            None => analysis.missing.push(member.package_name.clone()),
          }
        }
        analysis.inherited.sort();
        analysis.planned.sort();
        analysis.local_overrides.sort();
        analysis.missing.sort();
        analysis.retained_equivalent.sort();
        analysis
      })
      .collect()
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
        Item::Value(Value::InlineTable(inline_table)) => {
          Self::parse_dep_table(inline_table, dep_name, ctx.workspace_dependencies)?
        }
        Item::Table(table) => Self::parse_dep_table(table, dep_name, ctx.workspace_dependencies)?,
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
          local_features: p.local_features,
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
          workspace_inherited: p.workspace_inherited,
        };

        ctx.dependencies.entry(dep_key).or_default().push(usage);
      }
    }

    Ok(())
  }

  /// Parse a dependency table (either inline or full), resolving captured workspace inheritance.
  fn parse_dep_table<T: toml_edit::TableLike>(
    table: &T,
    dep_name: &str,
    workspace_dependencies: &FxHashMap<String, ExistingWorkspaceDep>,
  ) -> RailResult<Option<ParsedDepTable>> {
    let workspace_inherited = table.get("workspace").and_then(|value| value.as_bool()) == Some(true);
    let mut parsed = if workspace_inherited {
      let workspace = workspace_dependencies.get(dep_name).ok_or_else(|| {
        RailError::message(format!(
          "dependency '{dep_name}' inherits an absent [workspace.dependencies] entry"
        ))
      })?;
      ParsedDepTable {
        renamed_from: workspace
          .package
          .as_ref()
          .filter(|package| package.as_str() != dep_name)
          .map(|_| dep_name.to_string()),
        actual_name: workspace.package.clone(),
        unconditional_features: workspace.features.iter().cloned().collect(),
        default_features: workspace.default_features,
        optional: false,
        path: workspace.path.clone(),
        declared_version: workspace.version.clone(),
        workspace_inherited: true,
        ..Default::default()
      }
    } else {
      ParsedDepTable {
        default_features: true, // Cargo default
        ..Default::default()
      }
    };

    // Check for renamed package (only when package name differs from dependency key)
    if !workspace_inherited && let Some(pkg) = table.get("package").and_then(|v| v.as_str()) {
      // Only flag as renamed when there's actual renaming, not redundant package fields
      if pkg != dep_name {
        parsed.renamed_from = Some(dep_name.to_string());
        parsed.actual_name = Some(pkg.to_string());
      }
    }

    // Parse version - this is the DECLARED version from the manifest
    if !workspace_inherited {
      parsed.declared_version = table.get("version").and_then(|v| v.as_str()).map(String::from);
    }

    // Parse path for path dependencies
    if !workspace_inherited {
      parsed.path = table.get("path").and_then(|v| v.as_str()).map(String::from);
    }

    // Parse features
    if let Some(features) = table.get("features").and_then(|f| f.as_array()) {
      for feat in features {
        if let Some(s) = feat.as_str() {
          parsed.unconditional_features.insert(s.to_string());
          parsed.local_features.insert(s.to_string());
        }
      }
    }

    // Parse default-features
    if !workspace_inherited && let Some(df) = table.get("default-features").and_then(|v| v.as_bool()) {
      parsed.default_features = df;
    }

    // Parse optional
    if let Some(opt) = table.get("optional").and_then(|v| v.as_bool()) {
      parsed.optional = opt;
    }

    Ok(Some(parsed))
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

  /// Get unique package names from all dependencies
  ///
  /// Used to iterate by package rather than by dep key
  pub fn unique_packages(&self) -> HashSet<Arc<str>> {
    self.usage_index.keys().map(|k| Arc::clone(&k.name)).collect()
  }
}

// Workspace Dependencies Parser

fn package_field_retention_reason(field: &str) -> Option<&'static str> {
  match field {
    "version" => Some("release-policy-owned"),
    "license-file" | "readme" => Some("workspace-relative-path"),
    _ => None,
  }
}

pub(crate) fn package_field_is_automatically_inheritable(field: &str) -> bool {
  PACKAGE_INHERITANCE_FIELDS.contains(&field) && package_field_retention_reason(field).is_none()
}

fn parse_member_package_fields(
  doc: &DocumentMut,
  manifest_path: &Path,
) -> RailResult<BTreeMap<String, PackageFieldDeclaration>> {
  let mut fields = BTreeMap::new();
  let Some(package) = doc.get("package").and_then(Item::as_table_like) else {
    return Ok(fields);
  };
  for field in PACKAGE_INHERITANCE_FIELDS {
    let Some(item) = package.get(field) else {
      continue;
    };
    if item
      .as_table_like()
      .and_then(|table| table.get("workspace"))
      .and_then(Item::as_bool)
      == Some(true)
    {
      fields.insert(field.to_string(), PackageFieldDeclaration::Inherited);
      continue;
    }
    fields.insert(
      field.to_string(),
      PackageFieldDeclaration::Explicit(parse_package_field_value(item, field, manifest_path)?),
    );
  }
  Ok(fields)
}

fn parse_workspace_package_fields(
  doc: &DocumentMut,
  manifest_path: &Path,
) -> RailResult<BTreeMap<String, PackageFieldValue>> {
  let mut fields = BTreeMap::new();
  let Some(package) = doc
    .get("workspace")
    .and_then(|workspace| workspace.get("package"))
    .and_then(Item::as_table_like)
  else {
    return Ok(fields);
  };
  for field in PACKAGE_INHERITANCE_FIELDS {
    if let Some(item) = package.get(field) {
      fields.insert(
        field.to_string(),
        parse_package_field_value(item, field, manifest_path)?,
      );
    }
  }
  Ok(fields)
}

fn parse_package_field_value(item: &Item, field: &str, manifest_path: &Path) -> RailResult<PackageFieldValue> {
  if let Some(value) = item.as_str() {
    return Ok(PackageFieldValue::String(value.to_string()));
  }
  if let Some(value) = item.as_bool() {
    return Ok(PackageFieldValue::Boolean(value));
  }
  if let Some(values) = item.as_array() {
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
      let value = value.as_str().ok_or_else(|| {
        RailError::message(format!(
          "package field '{field}' in '{}' contains a non-string array value",
          manifest_path.display()
        ))
      })?;
      parsed.push(value.to_string());
    }
    return Ok(PackageFieldValue::Strings(parsed));
  }
  Err(RailError::message(format!(
    "package field '{field}' in '{}' has an unsupported declaration shape",
    manifest_path.display()
  )))
}

fn parse_workspace_manifest_policy(
  manifest_path: &Path,
  bytes: &[u8],
) -> RailResult<(
  FxHashMap<String, ExistingWorkspaceDep>,
  BTreeMap<String, PackageFieldValue>,
)> {
  let content = std::str::from_utf8(bytes)
    .map_err(|_| RailError::message(format!("Failed to parse {} as UTF-8", manifest_path.display())))?;
  let doc: DocumentMut = content
    .parse()
    .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
  let dependencies = parse_existing_workspace_deps_from_doc(&doc);
  let package_fields = parse_workspace_package_fields(&doc, manifest_path)?;
  Ok((dependencies, package_fields))
}

/// Information about an existing workspace dependency
#[derive(Debug, Clone)]
pub struct ExistingWorkspaceDep {
  /// Actual package name when the workspace entry is renamed.
  pub package: Option<String>,
  /// Version requirement (if specified)
  pub version: Option<String>,
  /// Features enabled
  pub features: Vec<String>,
  /// Whether default features are enabled
  pub default_features: bool,
  /// Path for path dependencies
  pub path: Option<String>,
}

/// Parse existing `[workspace.dependencies]` from captured root-manifest bytes.
///
/// Produces a map of dependency name to current workspace configuration.
/// This is used to detect deps that already exist in workspace.dependencies
/// so we don't add duplicates.
pub fn parse_existing_workspace_deps(
  manifest_path: &Path,
  bytes: &[u8],
) -> RailResult<FxHashMap<String, ExistingWorkspaceDep>> {
  parse_workspace_manifest_policy(manifest_path, bytes).map(|(dependencies, _)| dependencies)
}

fn parse_existing_workspace_deps_from_doc(doc: &DocumentMut) -> FxHashMap<String, ExistingWorkspaceDep> {
  let mut existing = FxHashMap::default();

  // Check for [workspace.dependencies] section
  let Some(workspace) = doc.get("workspace").and_then(|w| w.as_table()) else {
    return existing; // No [workspace] section
  };

  let Some(deps) = workspace.get("dependencies").and_then(|d| d.as_table_like()) else {
    return existing; // No [workspace.dependencies] section
  };

  for (name, value) in deps.iter() {
    let dep = parse_workspace_dep_entry(value);
    existing.insert(name.to_string(), dep);
  }

  existing
}

/// Parse a single workspace dependency entry
fn parse_workspace_dep_entry(value: &Item) -> ExistingWorkspaceDep {
  let mut dep = ExistingWorkspaceDep {
    package: None,
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
    if let Some(package) = table.get("package").and_then(|value| value.as_str()) {
      dep.package = Some(package.to_string());
    }
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
    if let Some(package) = table
      .get("package")
      .and_then(|item| item.as_value())
      .and_then(|value| value.as_str())
    {
      dep.package = Some(package.to_string());
    }
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

  fn parse_test_workspace(dir: &TempDir) -> FxHashMap<String, ExistingWorkspaceDep> {
    let path = dir.path().join("Cargo.toml");
    let bytes = std::fs::read(&path).unwrap();
    parse_existing_workspace_deps(&path, &bytes).unwrap()
  }

  #[test]
  fn test_parse_existing_workspace_deps_empty() {
    let dir = create_test_workspace(
      r#"
[workspace]
members = ["crate-a"]
"#,
    );

    let result = parse_test_workspace(&dir);
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

    let result = parse_test_workspace(&dir);
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

    let result = parse_test_workspace(&dir);
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

    let result = parse_test_workspace(&dir);
    assert_eq!(result.len(), 2);

    let crate_a = &result["crate-a"];
    assert_eq!(crate_a.path.as_ref().unwrap(), "crate-a");
    assert_eq!(crate_a.version.as_ref().unwrap(), "0.1.0");

    let external = &result["external"];
    assert!(external.path.is_none());
  }
}
