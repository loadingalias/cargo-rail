//! Unused dependency detection.
//!
//! Combines resolved-graph checks with target-aware rustc diagnostics.

use crate::cargo::manifest_analyzer::{DepKind, DepUsage, ManifestAnalyzer};
use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::unify_types::{MemberEdit, UnusedDep, UnusedReason};
use crate::compiler::CompilerDiagnosticsCollector;
use crate::compiler::cfg_eval::{TargetCfgSet, load_target_cfg_sets, target_constraint_matches_target};
use crate::progress;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// Detects unused dependencies in workspace members.
pub struct UnusedDepFinder<'a> {
  workspace_root: &'a Path,
  metadata: &'a MultiTargetMetadata,
  manifests: &'a ManifestAnalyzer,
  enable_compiler_diag_cache: bool,
}

impl<'a> UnusedDepFinder<'a> {
  /// Create a new unused dependency finder.
  pub fn new(
    workspace_root: &'a Path,
    metadata: &'a MultiTargetMetadata,
    manifests: &'a ManifestAnalyzer,
    enable_compiler_diag_cache: bool,
  ) -> Self {
    Self {
      workspace_root,
      metadata,
      manifests,
      enable_compiler_diag_cache,
    }
  }

  /// Detect unused dependencies in workspace members.
  pub fn find(&self) -> Vec<UnusedDep> {
    let mut unused = Vec::new();
    let source_unused = self.detect_source_unused_deps();

    let workspace_member_names: HashSet<String> = self
      .metadata
      .workspace_packages()
      .iter()
      .map(|pkg| pkg.name.to_string())
      .collect();

    let configured_targets: Vec<&str> = self.metadata.targets();
    let target_cfg_sets = self.load_target_cfg_sets(&configured_targets);
    let pkg_to_lib = self.metadata.package_to_lib_name_map();

    for member in &self.manifests.members {
      let resolved_deps = self.get_resolved_deps_for_member(&member.package_name);

      for (dep_key, usages) in &member.dependencies {
        if workspace_member_names.contains(&*dep_key.name) {
          continue;
        }

        let resolved_name = if dep_key.is_renamed() {
          dep_key.alias().replace('-', "_")
        } else {
          pkg_to_lib
            .get(&*dep_key.name)
            .cloned()
            .unwrap_or_else(|| dep_key.name.replace('-', "_"))
        };

        if resolved_deps.contains(&resolved_name) {
          for usage in usages {
            if self.should_skip_usage(usage, &configured_targets, &target_cfg_sets) || usage.kind != DepKind::Normal {
              continue;
            }

            let required_targets = usage_required_targets(usage, &configured_targets, &target_cfg_sets);
            if required_targets.is_empty() {
              continue;
            }

            let Some(member_diags) = source_unused.get(&member.package_name) else {
              continue;
            };
            if !member_diags.has_all_targets(&required_targets) {
              continue;
            }
            if !member_diags.has_complete_targets(&required_targets) {
              continue;
            }
            if !member_diags.is_unused_for_all_targets(&resolved_name, &required_targets) {
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

        for usage in usages {
          if self.should_skip_usage(usage, &configured_targets, &target_cfg_sets) {
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
  fn should_skip_usage(
    &self,
    usage: &DepUsage,
    configured_targets: &[&str],
    cfg_sets: &HashMap<String, TargetCfgSet>,
  ) -> bool {
    if usage.optional {
      return true;
    }

    if let Some(target_cfg) = &usage.target {
      return !target_constraint_matches_any(target_cfg, configured_targets, cfg_sets);
    }

    false
  }

  fn load_target_cfg_sets(&self, configured_targets: &[&str]) -> HashMap<String, TargetCfgSet> {
    match load_target_cfg_sets(self.workspace_root, configured_targets) {
      Ok(map) => map,
      Err(error) => {
        crate::warn!(
          "target cfg loading failed; target-specific dependency removals are disabled for safety: {}",
          error
        );
        HashMap::new()
      }
    }
  }

  /// Detect source-unused crates via rustc's `unused_crate_dependencies` lint.
  fn detect_source_unused_deps(&self) -> HashMap<String, crate::compiler::MemberDiagnostics> {
    if !has_source_check_candidates(self.manifests) {
      return HashMap::new();
    }

    let members: HashSet<&str> = self
      .manifests
      .members
      .iter()
      .map(|member| member.package_name.as_str())
      .collect();

    let targets = self.metadata.targets();
    let collector = match CompilerDiagnosticsCollector::new(
      self.workspace_root,
      self.manifests,
      targets,
      self.enable_compiler_diag_cache,
    ) {
      Ok(collector) => collector,
      Err(error) => {
        crate::warn!(
          "source-level unused dependency collector initialization failed; falling back to graph-only detection: {}",
          error
        );
        return HashMap::new();
      }
    };

    match collector.collect_for_members(&members) {
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

  /// Generate removal edits for unused dependencies.
  pub fn generate_removal_edits(&self, unused: &[UnusedDep]) -> HashMap<String, Vec<MemberEdit>> {
    let mut edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();

    for dep in unused {
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

  /// Get all resolved direct dependencies for a workspace member.
  fn get_resolved_deps_for_member(&self, member_name: &str) -> HashSet<String> {
    let mut deps = HashSet::new();

    for metadata in self.metadata.targets().iter().filter_map(|t| self.metadata.get(t)) {
      if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == member_name
          {
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

fn has_source_check_candidates(manifests: &ManifestAnalyzer) -> bool {
  manifests.members.iter().any(|member| {
    member
      .dependencies
      .values()
      .flatten()
      .any(|usage| usage.kind == DepKind::Normal && !usage.optional)
  })
}

fn usage_required_targets<'a>(
  usage: &DepUsage,
  configured_targets: &'a [&'a str],
  cfg_sets: &HashMap<String, TargetCfgSet>,
) -> Vec<&'a str> {
  match &usage.target {
    Some(target_cfg) => configured_targets
      .iter()
      .copied()
      .filter(|target| target_constraint_matches_target(target_cfg, target, cfg_sets.get(*target)))
      .collect(),
    None => configured_targets.to_vec(),
  }
}

/// Check if a target constraint (cfg expression) matches any configured target.
fn target_constraint_matches_any(
  cfg: &str,
  configured_targets: &[&str],
  cfg_sets: &HashMap<String, TargetCfgSet>,
) -> bool {
  configured_targets
    .iter()
    .copied()
    .any(|target| target_constraint_matches_target(cfg, target, cfg_sets.get(target)))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_target_constraint_matches_windows() {
    let targets = ["x86_64-pc-windows-msvc"];
    let mut cfg_sets = HashMap::new();
    cfg_sets.insert(
      "x86_64-pc-windows-msvc".to_string(),
      TargetCfgSet::from_test_lines(&["windows", "target_os=\"windows\""]),
    );
    assert!(target_constraint_matches_any("cfg(windows)", &targets, &cfg_sets));
    assert!(!target_constraint_matches_any("cfg(unix)", &targets, &cfg_sets));
  }

  #[test]
  fn test_target_constraint_matches_unix() {
    let targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"];
    let mut cfg_sets = HashMap::new();
    cfg_sets.insert(
      "x86_64-unknown-linux-gnu".to_string(),
      TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]),
    );
    cfg_sets.insert(
      "aarch64-apple-darwin".to_string(),
      TargetCfgSet::from_test_lines(&["unix", "target_os=\"macos\""]),
    );
    assert!(target_constraint_matches_any("cfg(unix)", &targets, &cfg_sets));
    assert!(!target_constraint_matches_any("cfg(windows)", &targets, &cfg_sets));
  }

  #[test]
  fn test_target_constraint_matches_specific_os() {
    let targets = ["x86_64-unknown-linux-gnu"];
    let mut cfg_sets = HashMap::new();
    cfg_sets.insert(
      "x86_64-unknown-linux-gnu".to_string(),
      TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]),
    );
    assert!(target_constraint_matches_any(
      "cfg(target_os = \"linux\")",
      &targets,
      &cfg_sets
    ));
    assert!(!target_constraint_matches_any(
      "cfg(target_os = \"macos\")",
      &targets,
      &cfg_sets
    ));
  }

  #[test]
  fn test_usage_required_targets_filters_by_cfg() {
    let usage = DepUsage {
      unconditional_features: std::collections::BTreeSet::new(),
      conditional_features: std::collections::BTreeSet::new(),
      default_features: true,
      kind: DepKind::Normal,
      target: Some("cfg(windows)".to_string()),
      used_by: Arc::from("crate"),
      optional: false,
      path: None,
      declared_version: Some("1".to_string()),
      manifest_path: None,
      cargo_toml_key: Arc::from("dep"),
      referenced_in_features: false,
    };

    let configured = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"];
    let mut cfg_sets = HashMap::new();
    cfg_sets.insert(
      "x86_64-unknown-linux-gnu".to_string(),
      TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]),
    );
    cfg_sets.insert(
      "x86_64-pc-windows-msvc".to_string(),
      TargetCfgSet::from_test_lines(&["windows", "target_os=\"windows\""]),
    );
    let required = usage_required_targets(&usage, &configured, &cfg_sets);
    assert_eq!(required, vec!["x86_64-pc-windows-msvc"]);
  }
}
