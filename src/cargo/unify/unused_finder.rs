//! Unused dependency detection
//!
//! Combines graph-level checks with rustc's source-level diagnostics.

use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::cargo::manifest_analyzer::{DepKind, DepUsage};
use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::unify_types::{MemberEdit, UnusedDep, UnusedReason};
use crate::error::ResultExt;
use crate::progress;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

/// Detects unused dependencies in workspace members
pub struct UnusedDepFinder<'a> {
  workspace_root: &'a Path,
  metadata: &'a MultiTargetMetadata,
  manifests: &'a ManifestAnalyzer,
}

impl<'a> UnusedDepFinder<'a> {
  /// Create a new unused dependency finder
  pub fn new(workspace_root: &'a Path, metadata: &'a MultiTargetMetadata, manifests: &'a ManifestAnalyzer) -> Self {
    Self {
      workspace_root,
      metadata,
      manifests,
    }
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
    let source_unused = self.detect_source_unused_deps();

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
      for (dep_key, usages) in &member.dependencies {
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
          // Source-level detection catches "declared + resolved but never referenced".
          // This solves the common single-crate case where graph-only checks miss dead deps.
          for usage in usages {
            if self.should_skip_usage(usage, &configured_targets) {
              continue;
            }
            if usage.kind != DepKind::Normal {
              continue;
            }
            let Some(member_unused) = source_unused.get(&member.package_name) else {
              continue;
            };
            if !member_unused.contains(&resolved_name) {
              continue;
            }
            unused.push(UnusedDep {
              member: Arc::from(member.package_name.as_str()),
              dep_name: Arc::clone(&dep_key.name),
              kind: usage.kind,
              reason: UnusedReason::NotUsedInSource,
            });
          }
          continue;
        }

        // Check each usage of this dep (can appear in multiple sections)
        for usage in usages {
          if self.should_skip_usage(usage, &configured_targets) {
            continue;
          }

          if let Some(target_cfg) = &usage.target {
            unused.push(UnusedDep {
              member: Arc::from(member.package_name.as_str()),
              dep_name: Arc::clone(&dep_key.name),
              kind: usage.kind,
              reason: UnusedReason::TargetConfiguredButNotResolved {
                target_cfg: Arc::from(target_cfg.as_str()),
              },
            });
            continue;
          }

          // This is a non-conditional, non-optional dep that's not in the resolved graph
          // This is genuinely unused
          unused.push(UnusedDep {
            member: Arc::from(member.package_name.as_str()),
            dep_name: Arc::clone(&dep_key.name),
            kind: usage.kind,
            reason: UnusedReason::NotInResolvedGraph,
          });
        }
      }
    }

    if !unused.is_empty() {
      progress!("  Found {} potentially unused dependencies", unused.len());
    }

    unused
  }

  /// Conservative skip rules to avoid false positives.
  fn should_skip_usage(&self, usage: &DepUsage, configured_targets: &[&str]) -> bool {
    // Optional deps may be used under cfg(feature = "...") without explicit references.
    if usage.optional {
      return true;
    }

    // If target isn't configured, we cannot verify usage accurately.
    if let Some(target_cfg) = &usage.target {
      return !target_constraint_matches_any(target_cfg, configured_targets);
    }

    false
  }

  /// Detect source-unused crates via rustc's `unused_crate_dependencies` lint.
  ///
  /// This is the only reliable source-level signal for dependencies that are
  /// declared/resolved but never referenced in Rust code.
  fn detect_source_unused_deps(&self) -> HashMap<String, HashSet<String>> {
    if !has_source_check_candidates(&self.manifests) {
      return HashMap::new();
    }

    let manifest_to_member = build_manifest_member_index(&self.manifests.members);
    if manifest_to_member.is_empty() {
      return HashMap::new();
    }

    match run_unused_crate_dependency_check(self.workspace_root, &manifest_to_member) {
      Ok(map) => map,
      Err(error) => {
        crate::warn!(
          "source-level unused dependency detection failed; falling back to graph-only detection: {}",
          error
        );
        HashMap::new()
      }
    }
  }

  /// Generate removal edits for unused dependencies
  pub fn generate_removal_edits(&self, unused: &[UnusedDep]) -> HashMap<String, Vec<MemberEdit>> {
    let mut edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();

    for dep in unused {
      // Find the target constraint for this dep (needed for correct section)
      let target: Option<Arc<str>> = self
        .manifests
        .members
        .iter()
        .find(|m| m.package_name == *dep.member)
        .and_then(|m| {
          m.dependencies.iter().find_map(|(key, usages)| {
            usages.iter().find_map(|usage| {
              if *key.name == *dep.dep_name && usage.kind == dep.kind {
                usage.target.as_ref().map(|t| Arc::from(t.as_str()))
              } else {
                None
              }
            })
          })
        });

      edits
        .entry(dep.member.to_string())
        .or_default()
        .push(MemberEdit::RemoveDep {
          dep_name: Arc::clone(&dep.dep_name),
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

fn build_manifest_member_index(members: &[crate::cargo::manifest_analyzer::ParsedManifest]) -> HashMap<String, String> {
  let mut index = HashMap::with_capacity(members.len() * 2);
  for member in members {
    index.insert(member.path.to_string_lossy().into_owned(), member.package_name.clone());
    if let Ok(canonical) = member.path.canonicalize() {
      index.insert(canonical.to_string_lossy().into_owned(), member.package_name.clone());
    }
  }
  index
}

fn has_source_check_candidates(manifests: &ManifestAnalyzer) -> bool {
  manifests.members.iter().any(|member| {
    member
      .dependencies
      .values()
      .flatten()
      .any(|usage| usage.kind == DepKind::Normal && !usage.optional && usage.target.is_none())
  })
}

fn run_unused_crate_dependency_check(
  workspace_root: &Path,
  manifest_to_member: &HashMap<String, String>,
) -> crate::error::RailResult<HashMap<String, HashSet<String>>> {
  let rustflags = std::env::var("RUSTFLAGS").ok();
  let lint_flag = "-Wunused-crate-dependencies";
  let merged_rustflags = match rustflags {
    Some(flags) if flags.split_whitespace().any(|flag| flag == lint_flag) => flags,
    Some(flags) => format!("{} {}", flags, lint_flag),
    None => lint_flag.to_string(),
  };

  let output = Command::new("cargo")
    .current_dir(workspace_root)
    .env("RUSTFLAGS", merged_rustflags)
    .args([
      "check",
      "--workspace",
      "--all-targets",
      "--all-features",
      "--message-format=json",
    ])
    .output()
    .with_context(|| format!("running cargo check in {}", workspace_root.display()))?;

  let mut by_member: HashMap<String, HashSet<String>> = HashMap::new();
  for line in String::from_utf8_lossy(&output.stdout).lines() {
    let Ok(message) = serde_json::from_str::<CargoCompilerMessage>(line) else {
      continue;
    };
    if message.reason != "compiler-message" {
      continue;
    }
    if message.message.code.as_ref().and_then(|c| c.code.as_deref()) != Some("unused_crate_dependencies") {
      continue;
    }
    let Some(manifest_path) = message.manifest_path.as_deref() else {
      continue;
    };
    let Some(member_name) = resolve_member_name(manifest_path, manifest_to_member) else {
      continue;
    };
    let Some(crate_name) = parse_unused_crate_name(&message.message.message) else {
      continue;
    };

    by_member
      .entry(member_name.to_string())
      .or_default()
      .insert(crate_name.replace('-', "_"));
  }

  Ok(by_member)
}

fn resolve_member_name<'a>(manifest_path: &str, manifest_to_member: &'a HashMap<String, String>) -> Option<&'a str> {
  manifest_to_member.get(manifest_path).map(String::as_str)
}

fn parse_unused_crate_name(message: &str) -> Option<&str> {
  let prefix = "extern crate `";
  let start = message.find(prefix)? + prefix.len();
  let rest = &message[start..];
  let end = rest.find('`')?;
  Some(&rest[..end])
}

#[derive(Debug, Deserialize)]
struct CargoCompilerMessage {
  reason: String,
  manifest_path: Option<String>,
  message: CargoDiagnostic,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnostic {
  message: String,
  code: Option<CargoDiagnosticCode>,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnosticCode {
  code: Option<String>,
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
