//! Unused dependency detection
//!
//! Compares declared dependencies (from Cargo.toml) against the resolved
//! cargo graph to find deps that are declared but never actually used.

use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::unify_types::{MemberEdit, UnusedDep, UnusedReason};
use crate::progress;
use std::collections::{HashMap, HashSet};

/// Detects unused dependencies in workspace members
pub struct UnusedDepFinder<'a> {
  metadata: &'a MultiTargetMetadata,
  manifests: &'a ManifestAnalyzer,
}

impl<'a> UnusedDepFinder<'a> {
  /// Create a new unused dependency finder
  pub fn new(metadata: &'a MultiTargetMetadata, manifests: &'a ManifestAnalyzer) -> Self {
    Self { metadata, manifests }
  }

  /// Detect unused dependencies in workspace members
  ///
  /// This implements smart filtering to avoid false positives:
  /// - Optional deps referenced in `[features]` are feature-gated, not unused
  /// - Optional deps referenced by OTHER workspace crates' features are not unused
  /// - Target-specific deps for unconfigured targets can't be verified
  /// - Only truly unreferenced deps are flagged
  pub fn find(&self) -> Vec<UnusedDep> {
    let mut unused = Vec::new();

    // Pre-compute workspace member names (these are never "unused")
    let workspace_member_names: HashSet<String> = self
      .metadata
      .workspace_packages()
      .iter()
      .map(|pkg| pkg.name.to_string())
      .collect();

    // Get configured targets for target constraint checking
    let configured_targets: Vec<&str> = self.metadata.targets();

    // Build package name -> library name mapping
    // This handles crates where the package name differs from the lib name,
    // e.g., `mopa-maintained` has lib name `mopa`, so Rust code uses `use mopa::...`
    // but Cargo.toml declares `mopa-maintained = "0.2"`.
    let pkg_to_lib = self.metadata.package_to_lib_name_map();

    // For each workspace member, check declared deps against resolved
    for member in &self.manifests.members {
      // Get resolved deps for this member from the metadata
      let resolved_deps = self.get_resolved_deps_for_member(&member.package_name);

      // Check each declared dependency
      for (dep_key, usage) in &member.dependencies {
        // Skip workspace member deps - they're always structurally "used"
        // Use &*dep_key.name to get &str for comparison
        if workspace_member_names.contains(&*dep_key.name) {
          continue;
        }

        // Check if this dep appears in the resolved graph for this member
        // For renamed deps (memmap = { package = "memmap2" }), the resolved graph uses
        // the alias ("memmap") not the package name ("memmap2").
        // Fall back to pkg_to_lib mapping for non-renamed deps where lib name differs.
        let resolved_name = if dep_key.is_renamed() {
          // Renamed deps: use the alias (Cargo.toml key) - normalized to underscores
          dep_key.alias().replace('-', "_")
        } else {
          // Non-renamed: check if package has different lib name, else normalize package name
          pkg_to_lib
            .get(&*dep_key.name)
            .cloned()
            .unwrap_or_else(|| dep_key.name.replace('-', "_"))
        };
        if resolved_deps.contains(&resolved_name) {
          continue; // Dep is resolved, definitely used
        }

        // === Smart filtering to avoid false positives ===

        // Filter 1: Optional deps are ALWAYS feature-gated
        // Cargo automatically creates an implicit feature for each optional dependency,
        // so code can use `#[cfg(feature = "dep_name")]` even without explicit [features] entries.
        // We cannot safely determine if an optional dep is "unused" without analyzing source code
        // for cfg attributes, so we conservatively skip all optional deps.
        if usage.optional {
          continue;
        }

        // Filter 2: Target-specific deps for unconfigured targets
        // If a dep has a target constraint (e.g., cfg(windows)) and we don't have
        // that target configured, we can't verify if it's used or not.
        if let Some(ref target_cfg) = usage.target {
          if !target_constraint_matches_any(target_cfg, &configured_targets) {
            // Target not configured - can't verify, assume it's valid
            continue;
          }

          // Target IS configured but dep still not in resolved graph
          // This is genuinely suspicious - flag it with context
          unused.push(UnusedDep {
            member: member.package_name.clone(),
            dep_name: dep_key.name.to_string(),
            kind: usage.kind,
            reason: UnusedReason::TargetConfiguredButNotResolved {
              target_cfg: target_cfg.clone(),
            },
          });
          continue;
        }

        // This is a non-conditional, non-optional dep that's not in the resolved graph
        // This is genuinely unused
        unused.push(UnusedDep {
          member: member.package_name.clone(),
          dep_name: dep_key.name.to_string(),
          kind: usage.kind,
          reason: UnusedReason::NotInResolvedGraph,
        });
      }
    }

    if !unused.is_empty() {
      progress!("  Found {} potentially unused dependencies", unused.len());
    }

    unused
  }

  /// Generate removal edits for unused dependencies
  pub fn generate_removal_edits(&self, unused: &[UnusedDep]) -> HashMap<String, Vec<MemberEdit>> {
    let mut edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();

    for dep in unused {
      // Find the target constraint for this dep (needed for correct section)
      let target = self
        .manifests
        .members
        .iter()
        .find(|m| m.package_name == dep.member)
        .and_then(|m| {
          m.dependencies.iter().find_map(|(key, usage)| {
            if *key.name == dep.dep_name && usage.kind == dep.kind {
              usage.target.clone()
            } else {
              None
            }
          })
        });

      edits
        .entry(dep.member.clone())
        .or_default()
        .push(MemberEdit::RemoveDep {
          dep_name: dep.dep_name.clone(),
          dep_kind: dep.kind,
          target,
        });
    }

    edits
  }

  /// Get all resolved direct dependencies for a workspace member
  ///
  /// Uses the resolved cargo metadata graph to find what deps are actually
  /// used by a member. This is the source of truth for dependency usage.
  ///
  /// Returns normalized names (hyphens converted to underscores) to match
  /// cargo's internal crate name format.
  fn get_resolved_deps_for_member(&self, member_name: &str) -> HashSet<String> {
    let mut deps = HashSet::new();

    // Check across all targets to ensure we catch platform-specific deps
    for metadata in self.metadata.targets().iter().filter_map(|t| self.metadata.get(t)) {
      if let Some(resolve) = &metadata.resolve {
        // Find the node for this member
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == member_name
          {
            // Collect all direct dependencies from this node
            // Note: cargo metadata uses normalized names (underscores)
            for dep in &node.deps {
              deps.insert(dep.name.clone());
            }
          }
        }
      }
    }

    deps
  }
}

/// Check if a target constraint (cfg expression) matches any configured target
///
/// This is a heuristic check - we look for common patterns in the cfg string
/// and match against the target triples we have configured.
pub fn target_constraint_matches_any(cfg: &str, configured_targets: &[&str]) -> bool {
  // Normalize the cfg string for matching
  let cfg_lower = cfg.to_lowercase();

  for target in configured_targets {
    let target_lower = target.to_lowercase();

    // Check common cfg patterns against target triple components
    // cfg(windows) matches x86_64-pc-windows-msvc
    if cfg_lower.contains("windows") && target_lower.contains("windows") {
      return true;
    }
    // cfg(unix) matches linux, darwin, freebsd, etc.
    if cfg_lower.contains("unix")
      && (target_lower.contains("linux")
        || target_lower.contains("darwin")
        || target_lower.contains("freebsd")
        || target_lower.contains("netbsd")
        || target_lower.contains("openbsd"))
    {
      return true;
    }
    // cfg(target_os = "linux") matches *-linux-*
    if cfg_lower.contains("linux") && target_lower.contains("linux") {
      return true;
    }
    // cfg(target_os = "macos") or cfg(target_os = "darwin")
    if (cfg_lower.contains("macos") || cfg_lower.contains("darwin")) && target_lower.contains("darwin") {
      return true;
    }
    // cfg(target_arch = "wasm32") matches wasm32-*
    if cfg_lower.contains("wasm32") && target_lower.contains("wasm32") {
      return true;
    }
    // cfg(target_arch = "x86_64")
    if cfg_lower.contains("x86_64") && target_lower.contains("x86_64") {
      return true;
    }
    // cfg(target_arch = "aarch64")
    if cfg_lower.contains("aarch64") && target_lower.contains("aarch64") {
      return true;
    }
    // cfg(target_os = "android")
    if cfg_lower.contains("android") && target_lower.contains("android") {
      return true;
    }
    // cfg(target_os = "ios")
    if cfg_lower.contains("ios") && target_lower.contains("ios") {
      return true;
    }
    // Exact target match: cfg(target = "x86_64-unknown-linux-gnu")
    if cfg_lower.contains(&target_lower) {
      return true;
    }
  }

  false
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_target_constraint_matches_windows() {
    let targets = ["x86_64-pc-windows-msvc"];
    assert!(target_constraint_matches_any("cfg(windows)", &targets));
    assert!(!target_constraint_matches_any("cfg(unix)", &targets));
  }

  #[test]
  fn test_target_constraint_matches_unix() {
    let targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"];
    assert!(target_constraint_matches_any("cfg(unix)", &targets));
    assert!(!target_constraint_matches_any("cfg(windows)", &targets));
  }

  #[test]
  fn test_target_constraint_matches_specific_os() {
    let targets = ["x86_64-unknown-linux-gnu"];
    assert!(target_constraint_matches_any("cfg(target_os = \"linux\")", &targets));
    assert!(!target_constraint_matches_any("cfg(target_os = \"macos\")", &targets));
  }
}
