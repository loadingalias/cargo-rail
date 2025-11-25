//! Multi-target metadata loading with clean caching
//!
//! This replaces the old WorkspaceMetadata that was confused about --all-features.
//! We load metadata per target (in parallel) and cache it for reuse.

use crate::error::{RailResult, ResultExt};
use cargo_metadata::{Metadata, MetadataCommand, Package};
use rayon::prelude::*;
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Multi-target metadata cache for the HYBRID approach
///
/// Loads metadata for each target in parallel WITHOUT --all-features.
/// This gives us accurate version resolution per target while avoiding
/// the maximal feature set problem.
#[derive(Clone)]
pub struct MultiTargetMetadata {
  /// Metadata per target (or "default" if no targets specified)
  cache: HashMap<String, Metadata>,
}

impl MultiTargetMetadata {
  /// Load metadata for all targets in parallel
  pub fn load_parallel(workspace_root: &Path, targets: &[String]) -> RailResult<Self> {
    let workspace_root = workspace_root.to_path_buf();

    // If no targets specified, load default metadata
    if targets.is_empty() {
      let metadata = Self::load_single_target(&workspace_root, None)?;
      let mut cache = HashMap::new();
      cache.insert("default".to_string(), metadata);
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
    let mut cache = HashMap::new();
    for result in results {
      let (target, metadata) = result?;
      cache.insert(target, metadata);
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
      cmd.other_options(vec!["--filter-platform".to_string(), target_triple.to_string()]);
    }

    // IMPORTANT: NO --all-features! We want cargo's default resolution
    // Features come from manifest analysis (intersection of unconditional)

    let metadata = cmd.exec().with_context(|| {
      if let Some(t) = target {
        format!("Failed to load cargo metadata for target '{}'", t)
      } else {
        "Failed to load cargo metadata".to_string()
      }
    })?;

    Ok(metadata)
  }

  /// Get metadata for a specific target
  pub fn get(&self, target: &str) -> Option<&Metadata> {
    self.cache.get(target)
  }

  /// Get metadata for any target (useful when they should all be the same)
  pub fn any(&self) -> Option<&Metadata> {
    self.cache.values().next()
  }

  /// Get all targets we have metadata for
  pub fn targets(&self) -> Vec<&str> {
    self.cache.keys().map(|s| s.as_str()).collect()
  }

  /// Get workspace packages (same across all targets)
  pub fn workspace_packages(&self) -> Vec<&Package> {
    self.any().map(|m| m.workspace_packages()).unwrap_or_default()
  }

  /// Get all versions of a dependency across targets (includes transitive deps)
  /// Returns map of target -> version
  /// NOTE: This includes transitive dependencies - use direct_dep_versions() for direct deps only
  pub fn all_versions(&self, dep_name: &str) -> HashMap<String, Version> {
    let mut versions = HashMap::new();

    for (target, metadata) in &self.cache {
      if let Some(resolve) = &metadata.resolve {
        // Find the package in the resolved graph
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == dep_name
          {
            versions.insert(target.clone(), pkg.version.clone());
            break; // Found it for this target
          }
        }
      }
    }

    versions
  }

  /// Get versions of a dependency that are DIRECT dependencies of workspace members only
  /// This filters out transitive dependencies, ensuring we only unify versions
  /// that workspace members explicitly depend on.
  /// Returns map of target -> version
  pub fn direct_dep_versions(&self, dep_name: &str) -> HashMap<String, Version> {
    let mut versions = HashMap::new();

    for (target, metadata) in &self.cache {
      // Get workspace member package IDs
      let workspace_member_ids: HashSet<_> = metadata.workspace_packages().iter().map(|p| &p.id).collect();

      if let Some(resolve) = &metadata.resolve {
        // Look at direct dependencies of workspace members only
        for node in &resolve.nodes {
          // Skip if not a workspace member
          if !workspace_member_ids.contains(&node.id) {
            continue;
          }

          // Check if this workspace member has dep_name as a direct dependency
          for dep in &node.deps {
            if dep.name == dep_name {
              // Found a direct dependency - get its version from packages
              if let Some(pkg) = metadata.packages.iter().find(|p| p.id == dep.pkg) {
                // Use the highest version if multiple workspace members depend on different versions
                let existing = versions.get(target);
                if existing.is_none() || pkg.version > *existing.unwrap() {
                  versions.insert(target.clone(), pkg.version.clone());
                }
              }
            }
          }
        }
      }
    }

    versions
  }

  /// Check if a dependency is transitive-only (never in direct deps)
  pub fn is_transitive_only(&self, dep_name: &str) -> bool {
    // Check all workspace packages to see if any directly depend on this
    for metadata in self.cache.values() {
      for pkg in metadata.workspace_packages() {
        for dep in &pkg.dependencies {
          if dep.name == dep_name {
            return false; // Found in direct deps
          }
        }
      }
    }

    // Check if it exists in the resolved graph at all
    for metadata in self.cache.values() {
      if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == dep_name
          {
            return true; // In graph but not direct = transitive
          }
        }
      }
    }

    false // Not in graph at all
  }

  /// Get features enabled for a package across all targets
  /// Returns map of target -> set of features
  pub fn all_features(&self, dep_name: &str) -> HashMap<String, HashSet<String>> {
    let mut features = HashMap::new();

    for (target, metadata) in &self.cache {
      if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
          // Find the package
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == dep_name
          {
            // Get the features for this node
            let feat_set: HashSet<String> = node
              .features
              .iter()
              .filter(|f| {
                // Filter out non-existent features (cargo metadata quirk)
                pkg.features.contains_key(f.as_str())
              })
              .map(|f| f.to_string())
              .collect();

            features.insert(target.clone(), feat_set);
            break;
          }
        }
      }
    }

    features
  }

  /// Check which targets include a specific dependency
  pub fn targets_with_dep(&self, dep_name: &str) -> Vec<String> {
    let mut targets = Vec::new();

    for (target, metadata) in &self.cache {
      if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == dep_name
          {
            targets.push(target.clone());
            break;
          }
        }
      }
    }

    targets
  }

  /// Detect transitive dependencies with fragmented features
  /// These are candidates for pinning (workspace-hack replacement)
  pub fn find_fragmented_transitives(&self) -> Vec<FragmentedTransitive> {
    let mut transitives = Vec::new();

    // Find all transitive-only deps
    let mut all_deps = HashSet::new();
    for metadata in self.cache.values() {
      if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id) {
            all_deps.insert(pkg.name.clone());
          }
        }
      }
    }

    for dep_name in all_deps {
      if !self.is_transitive_only(&dep_name) {
        continue; // Skip direct deps
      }

      let features = self.all_features(&dep_name);
      let unique_sets: HashSet<_> = features
        .values()
        .map(|set| set.iter().cloned().collect::<Vec<_>>())
        .collect();

      if unique_sets.len() > 1 {
        // This dep has different features across builds = fragmented
        let all_features: HashSet<String> = features.values().flat_map(|s| s.iter().cloned()).collect();

        transitives.push(FragmentedTransitive {
          name: dep_name.to_string(),
          feature_sets: features,
          unified_features: all_features.into_iter().collect(),
        });
      }
    }

    transitives
  }

  /// Compute the workspace MSRV from all resolved dependencies
  ///
  /// The MSRV is the maximum `rust-version` across all dependencies in the
  /// resolved graph. This ensures the workspace can build all its deps.
  ///
  /// Returns `None` if no dependencies specify rust-version.
  pub fn compute_msrv(&self) -> Option<ComputedMsrv> {
    let mut max_version: Option<Version> = None;
    let mut contributors: Vec<String> = Vec::new();
    let mut deps_with_msrv = 0;
    let mut seen_packages: HashSet<String> = HashSet::new();

    // Iterate through all packages in the resolved graph
    for metadata in self.cache.values() {
      for pkg in &metadata.packages {
        // Skip if we've already processed this package (may appear in multiple targets)
        let pkg_key = format!("{}@{}", pkg.name, pkg.version);
        if seen_packages.contains(&pkg_key) {
          continue;
        }
        seen_packages.insert(pkg_key);

        // Check if this package has rust-version specified
        if let Some(ref rust_version) = pkg.rust_version {
          deps_with_msrv += 1;

          match &max_version {
            None => {
              max_version = Some(rust_version.clone());
              contributors = vec![pkg.name.to_string()];
            }
            Some(current_max) => {
              if rust_version > current_max {
                max_version = Some(rust_version.clone());
                contributors = vec![pkg.name.to_string()];
              } else if rust_version == current_max {
                contributors.push(pkg.name.to_string());
              }
            }
          }
        }
      }
    }

    max_version.map(|version| ComputedMsrv {
      version,
      contributors,
      deps_with_msrv,
    })
  }
}

/// A transitive dependency with fragmented features across targets
#[derive(Debug, Clone)]
pub struct FragmentedTransitive {
  /// Dependency name
  pub name: String,
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
  /// The computed MSRV (maximum of all deps' rust-version)
  pub version: Version,
  /// Dependencies that contributed to the MSRV (those with the highest rust-version)
  pub contributors: Vec<String>,
  /// Total number of deps with rust-version specified
  pub deps_with_msrv: usize,
}
