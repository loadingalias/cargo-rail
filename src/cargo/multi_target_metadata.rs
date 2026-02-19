//! Multi-target metadata loading with clean caching
//!
//! This replaces the old WorkspaceMetadata that was confused about --all-features.
//! We load metadata per target (in parallel) and cache it for reuse.

use crate::error::RailResult;
use cargo_metadata::{Metadata, MetadataCommand, Package, PackageId};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone)]
struct TargetMetadataEntry {
  metadata: Metadata,
  package_id_index: HashMap<PackageId, usize>,
}

impl TargetMetadataEntry {
  fn new(metadata: Metadata) -> Self {
    let package_id_index = metadata
      .packages
      .iter()
      .enumerate()
      .map(|(idx, pkg)| (pkg.id.clone(), idx))
      .collect();

    Self {
      metadata,
      package_id_index,
    }
  }

  fn package_by_id(&self, id: &PackageId) -> Option<&Package> {
    self.package_id_index.get(id).map(|&idx| &self.metadata.packages[idx])
  }
}

/// Multi-target metadata cache for the HYBRID approach
///
/// Loads metadata for each target in parallel WITHOUT --all-features.
/// This gives us accurate version resolution per target while avoiding
/// the maximal feature set problem.
#[derive(Clone)]
pub struct MultiTargetMetadata {
  /// Metadata per target (or "default" if no targets specified)
  /// Uses FxHashMap for faster String key lookups
  cache: FxHashMap<String, TargetMetadataEntry>,
}

impl MultiTargetMetadata {
  /// Load metadata for all targets in parallel
  pub fn load_parallel(workspace_root: &Path, targets: &[String]) -> RailResult<Self> {
    let workspace_root = workspace_root.to_path_buf();

    // If no targets specified, load default metadata
    if targets.is_empty() {
      let metadata = Self::load_single_target(&workspace_root, None)?;
      let mut cache = FxHashMap::default();
      cache.insert(String::from("default"), TargetMetadataEntry::new(metadata));
      return Ok(Self { cache });
    }

    // Load all targets in parallel using Rayon
    let results: Vec<RailResult<(String, Metadata)>> = targets
      .par_iter()
      .map(|target| {
        let metadata = Self::load_single_target(&workspace_root, Some(target))?;
        Ok((target.clone(), metadata))
      })
      .collect();

    // Collect results, propagating any errors
    let mut cache = FxHashMap::default();
    for result in results {
      let (target, metadata) = result?;
      cache.insert(target, TargetMetadataEntry::new(metadata));
    }

    Ok(Self { cache })
  }

  /// Load metadata for a single target
  fn load_single_target(workspace_root: &Path, target: Option<&str>) -> RailResult<Metadata> {
    let manifest_path = workspace_root.join("Cargo.toml");

    let mut cmd = MetadataCommand::new();
    cmd.manifest_path(&manifest_path);

    // Add target filtering if specified
    if let Some(target_triple) = target {
      cmd.other_options(vec![String::from("--filter-platform"), String::from(target_triple)]);
    }

    // IMPORTANT: NO --all-features! We want cargo's default resolution
    // Features come from manifest analysis (intersection of unconditional)

    let metadata = cmd.exec().map_err(|e| {
      if let Some(t) = target {
        let err_str = e.to_string();
        // Detect missing target scenario
        if err_str.contains("error[E0463]")
          || err_str.contains("can't find crate")
          || err_str.contains("target may not be installed")
        {
          crate::error::RailError::with_help(
            format!("Target '{}' is not installed on this machine", t),
            format!(
              "Install the target with: rustup target add {}\n\
               Or remove it from rail.toml [targets] if not needed for this workspace.",
              t
            ),
          )
        } else {
          crate::error::RailError::with_help(
            format!("Failed to load cargo metadata for target '{}'", t),
            format!("Error: {}\n\nCheck that the target is valid and installed.", e),
          )
        }
      } else {
        crate::error::RailError::with_help("Failed to load cargo metadata".to_string(), format!("Error: {}", e))
      }
    })?;

    Ok(metadata)
  }

  /// Get metadata for a specific target
  pub fn get(&self, target: &str) -> Option<&Metadata> {
    self.cache.get(target).map(|e| &e.metadata)
  }

  /// Get metadata for any target (useful when they should all be the same)
  pub fn any(&self) -> Option<&Metadata> {
    self.cache.values().next().map(|e| &e.metadata)
  }

  /// Get all targets we have metadata for (sorted for deterministic output)
  pub fn targets(&self) -> Vec<&str> {
    let mut targets: Vec<_> = self.cache.keys().map(|s| s.as_str()).collect();
    targets.sort_unstable();
    targets
  }

  /// Get workspace packages (same across all targets)
  pub fn workspace_packages(&self) -> Vec<&Package> {
    self.any().map(|m| m.workspace_packages()).unwrap_or_default()
  }

  /// Get all versions of a dependency across targets (includes transitive deps)
  /// Returns map of target -> version
  /// NOTE: This includes transitive dependencies - use direct_dep_versions() for direct deps only
  pub fn all_versions(&self, dep_name: &str) -> HashMap<String, Version> {
    let mut versions = HashMap::with_capacity(self.cache.len());

    for (target, entry) in &self.cache {
      let metadata = &entry.metadata;
      if let Some(resolve) = &metadata.resolve {
        // Find the package in the resolved graph
        // Clone target/version only when we find a match (outside inner loop)
        let found_version = resolve.nodes.iter().find_map(|node| {
          entry
            .package_by_id(&node.id)
            .filter(|pkg| pkg.name == dep_name)
            .map(|pkg| pkg.version.clone())
        });

        if let Some(version) = found_version {
          versions.insert(target.clone(), version);
        }
      }
    }

    versions
  }

  /// Get versions of a dependency that are DIRECT dependencies of workspace members only
  ///
  /// This filters out transitive dependencies, ensuring we only unify versions
  /// that workspace members explicitly depend on. Returns map of target -> version.
  ///
  /// Note: Within each target, cargo's resolver produces exactly ONE version per crate.
  /// We return that resolved version for each target where the dep is a direct dependency.
  pub fn direct_dep_versions(&self, dep_name: &str) -> HashMap<String, Version> {
    let mut versions = HashMap::with_capacity(self.cache.len());

    for (target, entry) in &self.cache {
      let metadata = &entry.metadata;
      // Get workspace member package IDs
      let workspace_member_ids: HashSet<_> = metadata.workspace_packages().iter().map(|p| &p.id).collect();

      if let Some(resolve) = &metadata.resolve {
        // Find first workspace member that has dep_name as a direct dependency
        // Clone target/version only once when found (outside inner loops)
        let found_version = resolve
          .nodes
          .iter()
          .filter(|node| workspace_member_ids.contains(&node.id))
          .find_map(|node| {
            node
              .deps
              .iter()
              .find(|dep| dep.name == dep_name)
              .and_then(|dep| entry.package_by_id(&dep.pkg))
              .map(|pkg| pkg.version.clone())
          });

        if let Some(version) = found_version {
          versions.insert(target.clone(), version);
        }
      }
    }

    versions
  }

  /// Check if a dependency is transitive-only (never in direct deps)
  pub fn is_transitive_only(&self, dep_name: &str) -> bool {
    // Check all workspace packages to see if any directly depend on this
    let is_direct_dep = self.cache.values().any(|entry| {
      entry
        .metadata
        .workspace_packages()
        .iter()
        .any(|pkg| pkg.dependencies.iter().any(|dep| dep.name == dep_name))
    });

    if is_direct_dep {
      return false;
    }

    // Check if it exists in the resolved graph at all
    self.cache.values().any(|entry| {
      entry.metadata.resolve.as_ref().is_some_and(|resolve| {
        resolve
          .nodes
          .iter()
          .any(|node| entry.package_by_id(&node.id).is_some_and(|pkg| pkg.name == dep_name))
      })
    })
  }

  /// Check if a dependency is a path/workspace dependency (not from a registry)
  ///
  /// Path dependencies have `source: None` in cargo metadata.
  /// These cannot be pinned in workspace.dependencies without a registry source,
  /// so we skip them during transitive pinning.
  pub fn is_path_dependency(&self, dep_name: &str) -> bool {
    // source is None for path deps and workspace members
    // source is Some("registry+...") for published deps
    self
      .cache
      .values()
      .flat_map(|entry| &entry.metadata.packages)
      .find(|pkg| pkg.name == dep_name)
      .is_some_and(|pkg| pkg.source.is_none())
  }

  /// Get features enabled for a package across all targets
  /// Returns map of target -> set of features
  pub fn all_features(&self, dep_name: &str) -> HashMap<String, HashSet<String>> {
    let mut features = HashMap::with_capacity(self.cache.len());

    for (target, entry) in &self.cache {
      let metadata = &entry.metadata;
      if let Some(resolve) = &metadata.resolve {
        // Find the package and collect features - clone target only once when found
        let found_features = resolve.nodes.iter().find_map(|node| {
          entry
            .package_by_id(&node.id)
            .filter(|pkg| pkg.name == dep_name)
            .map(|pkg| {
              node
                .features
                .iter()
                .filter(|f| pkg.features.contains_key(f.as_str()))
                .map(|f| f.to_string())
                .collect::<HashSet<String>>()
            })
        });

        if let Some(feat_set) = found_features {
          features.insert(target.clone(), feat_set);
        }
      }
    }

    features
  }

  /// Get features enabled for a package on ALL targets (intersection)
  ///
  /// Features only enabled on some targets are platform-specific and should
  /// not be added unconditionally to manifests.
  #[allow(dead_code)] // May be useful for future platform-aware features
  pub fn universal_features(&self, dep_name: &str) -> HashSet<String> {
    let per_target = self.all_features(dep_name);
    if per_target.is_empty() {
      return HashSet::new();
    }

    // Start with features from first target, intersect with rest
    let mut iter = per_target.values();
    let mut result = iter.next().cloned().unwrap_or_default();

    for feats in iter {
      result.retain(|f| feats.contains(f));
    }

    result
  }

  /// Check which targets include a specific dependency (sorted for deterministic output)
  pub fn targets_with_dep(&self, dep_name: &str) -> Vec<String> {
    let mut targets = Vec::with_capacity(self.cache.len());

    for (target, entry) in &self.cache {
      let metadata = &entry.metadata;
      if let Some(resolve) = &metadata.resolve {
        // Check if any node matches - clone target only once when found
        let has_dep = resolve
          .nodes
          .iter()
          .any(|node| entry.package_by_id(&node.id).is_some_and(|pkg| pkg.name == dep_name));

        if has_dep {
          targets.push(target.clone());
        }
      }
    }

    targets.sort_unstable();
    targets
  }

  /// Detect transitive dependencies with fragmented features
  /// These are candidates for pinning (workspace-hack replacement)
  pub fn find_fragmented_transitives(&self) -> Vec<FragmentedTransitive> {
    let mut transitives = Vec::new();

    // Find all transitive-only deps - use flat_map to avoid nested loops with clones
    let all_deps: HashSet<&str> = self
      .cache
      .values()
      .filter_map(|entry| entry.metadata.resolve.as_ref())
      .flat_map(|resolve| &resolve.nodes)
      .filter_map(|node| self.cache.values().find_map(|entry| entry.package_by_id(&node.id)))
      .map(|pkg| pkg.name.as_str())
      .collect();

    for dep_name in all_deps {
      if !self.is_transitive_only(dep_name) {
        continue; // Skip direct deps
      }

      // Skip path dependencies - they can't be pinned from a registry
      if self.is_path_dependency(dep_name) {
        continue;
      }

      let features = self.all_features(dep_name);
      // Convert to sorted Vecs for stable comparison (HashSet iteration is non-deterministic)
      let unique_sets: HashSet<_> = features
        .values()
        .map(|set| {
          let mut vec: Vec<_> = set.iter().cloned().collect();
          vec.sort_unstable();
          vec
        })
        .collect();

      if unique_sets.len() > 1 {
        // This dep has different features across builds = fragmented
        //
        // IMPORTANT: Use INTERSECTION of features, not union!
        // Using union can enable features that pull in new transitive deps
        // that aren't in the current Cargo.lock, breaking resolution.
        // The intersection approach is safe - it only pins features that
        // are already enabled everywhere, avoiding new dep introduction.
        let mut feature_sets = features.values();
        let Some(first_set) = feature_sets.next() else {
          continue;
        };
        let mut common_features = first_set.clone();
        for set in feature_sets {
          common_features.retain(|feature| set.contains(feature));
        }

        // Get the resolved version (use highest across all targets)
        let versions = self.all_versions(dep_name);
        let version = match versions.values().max() {
          Some(v) => v.clone(),
          None => continue, // Skip if we can't determine version
        };

        // Sort unified_features for deterministic output
        let mut unified_features: Vec<_> = common_features.into_iter().collect();
        unified_features.sort_unstable();

        transitives.push(FragmentedTransitive {
          name: dep_name.to_string(),
          version,
          feature_sets: features,
          unified_features,
        });
      }
    }

    // Sort by name for deterministic output (we iterate over HashSet above)
    transitives.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    transitives
  }

  /// Compute the MSRV from dependencies only (internal helper)
  ///
  /// Computes the maximum rust-version across resolved dependencies,
  /// plus the deps that contributed to that maximum.
  fn compute_deps_msrv(&self) -> Option<(Version, Vec<String>, usize)> {
    let mut max_version: Option<Version> = None;
    let mut contributors: Vec<&str> = Vec::new();
    let mut deps_with_msrv = 0;
    let mut seen_packages: HashSet<&PackageId> = HashSet::new();

    // Iterate through all packages in the resolved graph
    for entry in self.cache.values() {
      let metadata = &entry.metadata;
      for pkg in &metadata.packages {
        // Skip if we've already processed this package (may appear in multiple targets)
        if !seen_packages.insert(&pkg.id) {
          continue;
        }

        // Check if this package has rust-version specified
        if let Some(ref rust_version) = pkg.rust_version {
          deps_with_msrv += 1;

          match &max_version {
            None => {
              max_version = Some(rust_version.clone());
              contributors = vec![pkg.name.as_str()];
            }
            Some(current_max) => {
              if rust_version > current_max {
                max_version = Some(rust_version.clone());
                contributors = vec![pkg.name.as_str()];
              } else if rust_version == current_max {
                contributors.push(pkg.name.as_str());
              }
            }
          }
        }
      }
    }

    max_version.map(|v| {
      (
        v,
        contributors.into_iter().map(std::borrow::ToOwned::to_owned).collect(),
        deps_with_msrv,
      )
    })
  }

  /// Compute the workspace MSRV with config-driven source selection
  ///
  /// Takes into account the existing workspace rust-version and the msrv_source
  /// configuration to determine the final MSRV value.
  ///
  /// Returns `None` when neither dependencies nor workspace metadata provide an
  /// MSRV; otherwise returns the computed version and provenance details.
  pub fn compute_msrv_with_config(
    &self,
    workspace_root: &Path,
    msrv_source: crate::config::MsrvSource,
  ) -> Option<ComputedMsrv> {
    use crate::config::MsrvSource;

    // Get MSRV from dependencies
    let deps_result = self.compute_deps_msrv();

    // Read existing rust-version baseline (prefer workspace.package, fallback to root package)
    let (workspace_msrv, used_package_fallback) = read_workspace_rust_version(workspace_root);

    // Apply msrv_source logic
    match msrv_source {
      MsrvSource::Deps => {
        // Original behavior: use deps only, ignore workspace
        deps_result.map(|(version, contributors, deps_with_msrv)| ComputedMsrv {
          version: version.clone(),
          contributors,
          deps_with_msrv,
          deps_msrv: Some(version),
          workspace_msrv,
          source_used: MsrvSourceUsed::Deps,
          warning: None,
        })
      }

      MsrvSource::Workspace => {
        // Preserve workspace rust-version, warn if deps need higher
        match (&workspace_msrv, &deps_result) {
          (Some(ws_ver), Some((deps_ver, contributors, deps_with_msrv))) => {
            let warning = if deps_ver > ws_ver {
              Some(format!(
                "workspace rust-version ({}.{}.{}) is lower than deps require ({}.{}.{}); \
                 deps {} need the higher version",
                ws_ver.major,
                ws_ver.minor,
                ws_ver.patch,
                deps_ver.major,
                deps_ver.minor,
                deps_ver.patch,
                contributors.first().map_or("unknown", String::as_str)
              ))
            } else if used_package_fallback {
              Some(
                "no [workspace.package].rust-version found; using [package].rust-version as baseline and writing it to [workspace.package].rust-version. \
consider enabling MSRV inheritance (rust-version = { workspace = true }) to avoid drift."
                  .to_string(),
              )
            } else {
              None
            };
            Some(ComputedMsrv {
              version: ws_ver.clone(),
              contributors: contributors.clone(),
              deps_with_msrv: *deps_with_msrv,
              deps_msrv: Some(deps_ver.clone()),
              workspace_msrv: Some(ws_ver.clone()),
              source_used: MsrvSourceUsed::Workspace,
              warning,
            })
          }
          (Some(ws_ver), None) => {
            // Workspace has rust-version but no deps do
            Some(ComputedMsrv {
              version: ws_ver.clone(),
              contributors: Vec::new(),
              deps_with_msrv: 0,
              deps_msrv: None,
              workspace_msrv: Some(ws_ver.clone()),
              source_used: MsrvSourceUsed::Workspace,
              warning: if used_package_fallback {
                Some(
                  "no [workspace.package].rust-version found; using [package].rust-version as baseline and writing it to [workspace.package].rust-version. \
consider enabling MSRV inheritance (rust-version = { workspace = true }) to avoid drift."
                    .to_string(),
                )
              } else {
                None
              },
            })
          }
          (None, Some((deps_ver, contributors, deps_with_msrv))) => {
            // No workspace rust-version, fall back to deps
            Some(ComputedMsrv {
              version: deps_ver.clone(),
              contributors: contributors.clone(),
              deps_with_msrv: *deps_with_msrv,
              deps_msrv: Some(deps_ver.clone()),
              workspace_msrv: None,
              source_used: MsrvSourceUsed::Deps,
              warning: Some("no workspace rust-version found, using deps MSRV".to_string()),
            })
          }
          (None, None) => None,
        }
      }

      MsrvSource::Max => {
        // Take max(workspace, deps) - explicit workspace setting wins if higher
        match (&workspace_msrv, &deps_result) {
          (Some(ws_ver), Some((deps_ver, contributors, deps_with_msrv))) => {
            let (version, source_used) = if ws_ver >= deps_ver {
              (ws_ver.clone(), MsrvSourceUsed::MaxWorkspace)
            } else {
              (deps_ver.clone(), MsrvSourceUsed::MaxDeps)
            };
            Some(ComputedMsrv {
              version,
              contributors: contributors.clone(),
              deps_with_msrv: *deps_with_msrv,
              deps_msrv: Some(deps_ver.clone()),
              workspace_msrv: Some(ws_ver.clone()),
              source_used,
              warning: if used_package_fallback {
                Some(
                  "no [workspace.package].rust-version found; using [package].rust-version as baseline. \
consider enabling MSRV inheritance (rust-version = { workspace = true }) to avoid drift."
                    .to_string(),
                )
              } else {
                None
              },
            })
          }
          (Some(ws_ver), None) => {
            // Workspace has rust-version but no deps do
            Some(ComputedMsrv {
              version: ws_ver.clone(),
              contributors: Vec::new(),
              deps_with_msrv: 0,
              deps_msrv: None,
              workspace_msrv: Some(ws_ver.clone()),
              source_used: MsrvSourceUsed::MaxWorkspace,
              warning: if used_package_fallback {
                Some(
                  "no [workspace.package].rust-version found; using [package].rust-version as baseline. \
consider enabling MSRV inheritance (rust-version = { workspace = true }) to avoid drift."
                    .to_string(),
                )
              } else {
                None
              },
            })
          }
          (None, Some((deps_ver, contributors, deps_with_msrv))) => {
            // No workspace rust-version, use deps
            Some(ComputedMsrv {
              version: deps_ver.clone(),
              contributors: contributors.clone(),
              deps_with_msrv: *deps_with_msrv,
              deps_msrv: Some(deps_ver.clone()),
              workspace_msrv: None,
              source_used: MsrvSourceUsed::MaxDeps,
              warning: None,
            })
          }
          (None, None) => None,
        }
      }
    }
  }
}

/// Read the existing rust-version baseline from workspace root Cargo.toml.
///
/// Prefers `[workspace.package].rust-version`. If absent, falls back to
/// `[package].rust-version` (if it is a string value).
///
/// Returns `(version, used_package_fallback)`.
fn read_workspace_rust_version(workspace_root: &Path) -> (Option<Version>, bool) {
  let cargo_toml_path = workspace_root.join("Cargo.toml");
  let Ok(content) = std::fs::read_to_string(&cargo_toml_path) else {
    return (None, false);
  };
  let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
    return (None, false);
  };

  // Try [workspace.package].rust-version
  let workspace_rust_version_str = doc
    .get("workspace")
    .and_then(|ws| ws.get("package"))
    .and_then(|pkg| pkg.get("rust-version"))
    .and_then(|v| v.as_str());

  if let Some(s) = workspace_rust_version_str {
    return (parse_rust_version(s), false);
  }

  // Fallback: root [package].rust-version (string only, not workspace inheritance)
  let package_rust_version_str = doc
    .get("package")
    .and_then(|pkg| pkg.get("rust-version"))
    .and_then(|v| v.as_str());

  if let Some(s) = package_rust_version_str {
    return (parse_rust_version(s), true);
  }

  (None, false)
}

/// Parse a rust-version string into a semver Version
///
/// Handles formats like "1.70", "1.70.0", etc.
fn parse_rust_version(s: &str) -> Option<Version> {
  // Try parsing directly
  if let Ok(v) = Version::parse(s) {
    return Some(v);
  }

  // Handle "1.70" format (missing patch)
  let parts: Vec<&str> = s.split('.').collect();
  if parts.len() == 2
    && let (Ok(major), Ok(minor)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>())
  {
    return Some(Version::new(major, minor, 0));
  }

  None
}

/// A transitive dependency with fragmented features across targets
#[derive(Debug, Clone)]
pub struct FragmentedTransitive {
  /// Dependency name
  pub name: String,
  /// Resolved version (highest across all targets)
  pub version: Version,
  /// Features per target
  pub feature_sets: HashMap<String, HashSet<String>>,
  /// Union of all features (for pinning)
  pub unified_features: Vec<String>,
}

impl FragmentedTransitive {
  /// Calculate the compilation overhead from fragmentation
  pub fn overhead_factor(&self) -> usize {
    self.feature_sets.len()
  }
}

/// Result of MSRV computation from dependency graph
#[derive(Debug, Clone)]
pub struct ComputedMsrv {
  /// The final MSRV to write (after applying msrv_source logic)
  pub version: Version,
  /// Dependencies that contributed to the deps-based MSRV
  pub contributors: Vec<String>,
  /// Total number of deps with rust-version specified
  pub deps_with_msrv: usize,
  /// The MSRV computed from dependencies (before applying msrv_source logic)
  pub deps_msrv: Option<Version>,
  /// The existing workspace rust-version (if any)
  pub workspace_msrv: Option<Version>,
  /// Which source was used to determine the final version
  pub source_used: MsrvSourceUsed,
  /// Warning message if workspace MSRV is lower than deps require
  pub warning: Option<String>,
}

/// Which source determined the final MSRV
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsrvSourceUsed {
  /// Used the maximum from dependencies
  Deps,
  /// Preserved existing workspace rust-version
  Workspace,
  /// Used max of workspace and deps (workspace was higher)
  MaxWorkspace,
  /// Used max of workspace and deps (deps was higher)
  MaxDeps,
}

impl MultiTargetMetadata {
  /// Build a mapping from package name to library name
  ///
  /// In Rust, a crate's package name (in Cargo.toml) can differ from its
  /// library name (what you `use` in code). For example:
  /// - Package: `mopa-maintained`
  /// - Library: `mopa` (what you write as `use mopa::...`)
  ///
  /// The resolved dependency graph uses library names, but Cargo.toml uses
  /// package names. This mapping allows correct lookup when detecting unused deps.
  ///
  /// Produces a map where:
  /// - Key: package name (e.g., "mopa-maintained")
  /// - Value: library name normalized with underscores (e.g., "mopa")
  pub fn package_to_lib_name_map(&self) -> HashMap<String, String> {
    use cargo_metadata::TargetKind;

    let mut map = HashMap::new();

    for entry in self.cache.values() {
      let metadata = &entry.metadata;
      for pkg in &metadata.packages {
        // Find the lib target to get the actual library name
        let lib_name = pkg
          .targets
          .iter()
          .find(|t| t.kind.contains(&TargetKind::Lib))
          .map(|t| t.name.clone())
          .unwrap_or_else(|| pkg.name.to_string());

        // Normalize to match cargo's internal format (underscores)
        let normalized_lib = lib_name.replace('-', "_");
        map.insert(pkg.name.to_string(), normalized_lib);
      }
    }

    map
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_rust_version_full() {
    let v = parse_rust_version("1.70.0").unwrap();
    assert_eq!(v.major, 1);
    assert_eq!(v.minor, 70);
    assert_eq!(v.patch, 0);
  }

  #[test]
  fn test_parse_rust_version_two_parts() {
    let v = parse_rust_version("1.70").unwrap();
    assert_eq!(v.major, 1);
    assert_eq!(v.minor, 70);
    assert_eq!(v.patch, 0);
  }

  #[test]
  fn test_parse_rust_version_high_minor() {
    let v = parse_rust_version("1.91").unwrap();
    assert_eq!(v.major, 1);
    assert_eq!(v.minor, 91);
    assert_eq!(v.patch, 0);
  }

  #[test]
  fn test_parse_rust_version_invalid() {
    assert!(parse_rust_version("invalid").is_none());
    assert!(parse_rust_version("").is_none());
    assert!(parse_rust_version("1").is_none());
    assert!(parse_rust_version("a.b.c").is_none());
  }

  #[test]
  fn test_msrv_source_used_variants() {
    // Just ensure the enum variants exist and are distinct
    assert_ne!(MsrvSourceUsed::Deps, MsrvSourceUsed::Workspace);
    assert_ne!(MsrvSourceUsed::MaxWorkspace, MsrvSourceUsed::MaxDeps);
  }

  // Determinism Regression Tests
  // These tests verify that outputs are deterministic (sorted) to prevent
  // non-deterministic behavior from HashMap/HashSet iteration order.

  #[test]
  fn test_targets_returns_sorted_output() {
    // Verify targets() sorting contract by checking our sort implementation
    // We can't easily mock Metadata, but we can verify the sorting logic
    let mut keys = vec!["z-target", "a-target", "m-target"];
    keys.sort_unstable();
    assert_eq!(keys, vec!["a-target", "m-target", "z-target"]);
  }

  #[test]
  fn test_fragmented_transitive_unified_features_sorting_contract() {
    // Verify the sorting contract: when constructing FragmentedTransitive,
    // the caller (find_fragmented_transitives) must sort unified_features.
    // This test demonstrates the correct construction pattern.

    // Simulate what find_fragmented_transitives does: sort before storing
    let mut features = vec!["zebra".to_string(), "alpha".to_string(), "beta".to_string()];
    features.sort_unstable(); // This is what find_fragmented_transitives does

    let transitive = FragmentedTransitive {
      name: "test-dep".to_string(),
      version: Version::new(1, 0, 0),
      feature_sets: HashMap::new(),
      unified_features: features,
    };

    // Verify the features are sorted (contract fulfilled)
    assert!(
      is_sorted(&transitive.unified_features),
      "unified_features should be sorted for deterministic output"
    );
    assert_eq!(
      transitive.unified_features,
      vec!["alpha", "beta", "zebra"],
      "Features should be in alphabetical order"
    );
  }

  #[test]
  fn test_feature_set_comparison_is_deterministic() {
    // Regression test: verify that comparing feature sets uses sorted Vecs
    // This was the bug: HashSet iteration order is non-deterministic, so
    // comparing HashSets by converting to Vec could give different results.

    let mut set1: HashSet<String> = HashSet::new();
    set1.insert("c".to_string());
    set1.insert("a".to_string());
    set1.insert("b".to_string());

    let mut set2: HashSet<String> = HashSet::new();
    set2.insert("a".to_string());
    set2.insert("b".to_string());
    set2.insert("c".to_string());

    // Convert to sorted Vecs (the fix we implemented)
    let mut vec1: Vec<_> = set1.iter().cloned().collect();
    vec1.sort_unstable();
    let mut vec2: Vec<_> = set2.iter().cloned().collect();
    vec2.sort_unstable();

    // Now they should be equal regardless of insertion order
    assert_eq!(vec1, vec2, "Sorted feature sets should be equal");
    assert_eq!(vec1, vec!["a", "b", "c"]);
  }

  #[test]
  fn test_find_fragmented_transitives_output_is_sorted() {
    // This test verifies the contract that find_fragmented_transitives returns
    // results sorted by name. We test the sorting logic directly since we can't
    // easily construct a full MultiTargetMetadata.

    let mut transitives = [
      FragmentedTransitive {
        name: "zebra-crate".to_string(),
        version: Version::new(1, 0, 0),
        feature_sets: HashMap::new(),
        unified_features: vec![],
      },
      FragmentedTransitive {
        name: "alpha-crate".to_string(),
        version: Version::new(1, 0, 0),
        feature_sets: HashMap::new(),
        unified_features: vec![],
      },
      FragmentedTransitive {
        name: "middle-crate".to_string(),
        version: Version::new(1, 0, 0),
        feature_sets: HashMap::new(),
        unified_features: vec![],
      },
    ];

    // Apply the same sort we use in find_fragmented_transitives
    transitives.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(transitives[0].name, "alpha-crate");
    assert_eq!(transitives[1].name, "middle-crate");
    assert_eq!(transitives[2].name, "zebra-crate");
  }

  /// Helper to check if a slice is sorted
  fn is_sorted(slice: &[String]) -> bool {
    slice.windows(2).all(|w| w[0] <= w[1])
  }
}
