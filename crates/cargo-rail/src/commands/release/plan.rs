//! Release planning: analyze changes and suggest version bumps
//!
//! The plan command:
//! 1. Loads workspace metadata via cargo_metadata
//! 2. Detects changed packages since last release tag
//! 3. Parses conventional commits to understand change types
//! 4. Runs cargo-semver-checks to detect API breaking changes
//! 5. Suggests version bumps (major/minor/patch)
//! 6. Computes publish order via dependency graph
//! 7. Outputs plan (table or JSON)

use crate::commands::release::changelog;
use crate::commands::release::graph::CrateGraph;
use crate::commands::release::semver::BumpType;
use crate::commands::release::semver_check;
use crate::commands::release::tags;
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use crate::ui::progress::MultiProgress;
use cargo_metadata::MetadataCommand;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;

/// A release plan for a single crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CratePlan {
  /// Crate name
  pub name: String,
  /// Current version
  pub current_version: String,
  /// Suggested next version
  pub next_version: String,
  /// Version bump type
  pub bump_type: BumpType,
  /// Reason for the bump
  pub reason: String,
  /// Has changes since last release
  pub has_changes: bool,
}

/// Complete release plan for workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePlan {
  /// Individual crate plans
  pub crates: Vec<CratePlan>,
  /// Publish order (dependencies first)
  pub publish_order: Vec<String>,
}

impl ReleasePlan {
  /// Filter plan to only crates with changes
  pub fn only_changed(&self) -> Self {
    Self {
      crates: self.crates.iter().filter(|c| c.has_changes).cloned().collect(),
      publish_order: self
        .publish_order
        .iter()
        .filter(|name| self.crates.iter().any(|c| &c.name == *name && c.has_changes))
        .cloned()
        .collect(),
    }
  }

  /// Output as human-readable table
  pub fn format_table(&self) -> String {
    let mut output = String::from("📦 Release Plan\n\n");

    if self.crates.is_empty() {
      output.push_str("No crates need to be released.\n");
      return output;
    }

    output.push_str("Package           Current    Next       Reason\n");
    output.push_str("─────────────────────────────────────────────────────────────────\n");

    for crate_plan in &self.crates {
      output.push_str(&format!(
        "{:<17} {:<10} {:<10} {}\n",
        crate_plan.name, crate_plan.current_version, crate_plan.next_version, crate_plan.reason
      ));
    }

    output.push_str(&format!("\nPublish order: {}\n", self.publish_order.join(" → ")));

    output
  }

  /// Output as JSON for CI
  pub fn to_json(&self) -> RailResult<String> {
    Ok(serde_json::to_string_pretty(self).map_err(|e| anyhow::anyhow!("JSON serialization failed: {}", e))?)
  }
}

/// Generate a release plan without printing (for programmatic use)
///
/// Returns a complete ReleasePlan with version bump suggestions for all workspace crates.
/// Callers can filter the plan using `ReleasePlan::only_changed()` if desired.
pub fn generate_release_plan(show_progress: bool) -> RailResult<ReleasePlan> {
  // Load workspace metadata
  let metadata = load_workspace_metadata()?;
  let workspace_root = metadata.workspace_root.as_std_path();

  // Try to load rail config to check publish settings (optional - may not exist for standalone repos)
  let publishable_crates: HashSet<String> = match RailConfig::load(workspace_root) {
    Ok(config) => {
      // Build set of crate names that have publish=true (or default true)
      config
        .splits
        .iter()
        .filter(|split| split.publish)
        .map(|split| split.name.clone())
        .collect()
    }
    Err(_) => {
      // No config found - treat all crates as publishable (standalone repo mode)
      HashSet::new()
    }
  };

  // Build dependency graph for publish ordering (workspace packages only)
  let mut workspace_pkgs: Vec<_> = metadata.workspace_packages().iter().cloned().cloned().collect();

  // Filter to publishable crates if config exists
  if !publishable_crates.is_empty() {
    workspace_pkgs.retain(|pkg| publishable_crates.contains(&pkg.name.to_string()));
  }

  let graph = CrateGraph::from_workspace(&workspace_pkgs)?;
  let publish_order = graph.topological_order()?;

  // Collect crate names and paths for change detection
  let crate_names: Vec<String> = workspace_pkgs.iter().map(|pkg| pkg.name.to_string()).collect();
  let crate_paths: HashMap<String, String> = workspace_pkgs
    .iter()
    .map(|pkg| {
      let relative_path = pkg
        .manifest_path
        .parent()
        .and_then(|p| p.strip_prefix(&metadata.workspace_root).ok())
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| ".".to_string());
      (pkg.name.to_string(), relative_path)
    })
    .collect();

  // Find last release tags for each crate
  let last_tags = tags::find_last_release_tags(workspace_root, &crate_names).unwrap_or_default();

  // Detect which crates have changed since last release
  let changed_crates = tags::detect_changed_crates(workspace_root, &crate_names, &crate_paths).unwrap_or_else(|e| {
    // If change detection fails, treat all crates as changed
    eprintln!("Warning: Failed to detect changes: {}", e);
    crate_names.iter().map(|name| (name.clone(), true)).collect()
  });

  // Create plans for each crate by analyzing commits (in parallel with progress tracking)
  if show_progress {
    println!("🔍 Analyzing {} crates in parallel...\n", workspace_pkgs.len());
  }

  let multi_progress = if show_progress {
    Some(MultiProgress::new())
  } else {
    None
  };
  let bars: Vec<_> = if let Some(ref mp) = multi_progress {
    workspace_pkgs
      .iter()
      .map(|pkg| mp.add_bar(1, format!("Analyzing {}", pkg.name)))
      .collect()
  } else {
    vec![]
  };

  let crate_plans: Vec<CratePlan> = workspace_pkgs
    .par_iter()
    .enumerate()
    .map(|(idx, pkg)| {
      let crate_name = pkg.name.as_str();
      let has_changes = changed_crates.get(crate_name).copied().unwrap_or(false);

      // Analyze commits to determine version bump
      let (bump_type, reason) = if has_changes {
        let last_commit = last_tags.get(crate_name).map(|tag| tag.commit_sha.as_str());
        let default_path = String::from(".");
        let crate_path = crate_paths.get(crate_name).unwrap_or(&default_path);

        match changelog::analyze_commits_for_crate(workspace_root, crate_path, last_commit) {
          Ok((commit_bump, commit_count)) => {
            // Also check API changes with cargo-semver-checks (if available and has baseline)
            let api_bump = if let Some(last_tag) = last_tags.get(crate_name) {
              let crate_abs_path = workspace_root.join(crate_path);
              match semver_check::check_api_changes(&crate_abs_path, &last_tag.version.to_string()) {
                Ok(Some(report)) => {
                  if report.has_major {
                    eprintln!(
                      "⚠️  {} has API breaking changes detected by cargo-semver-checks",
                      crate_name
                    );
                  }
                  report.suggested_bump
                }
                Ok(None) => {
                  // cargo-semver-checks not available or failed, skip API check
                  BumpType::None
                }
                Err(e) => {
                  eprintln!("Warning: cargo-semver-checks failed for {}: {}", crate_name, e);
                  BumpType::None
                }
              }
            } else {
              BumpType::None
            };

            // Combine commit-based bump and API-based bump (take the larger)
            let combined_bump = commit_bump.combine(api_bump);

            // Generate reason text
            let reason = if combined_bump == BumpType::None && commit_count > 0 {
              format!("{} non-version-bumping changes", commit_count)
            } else if commit_count == 0 {
              "New crate (no previous release)".to_string()
            } else {
              let mut reason_parts = Vec::new();

              // Add commit-based reason
              match commit_bump {
                BumpType::Major => reason_parts.push(format!("Breaking changes ({} commits)", commit_count)),
                BumpType::Minor => reason_parts.push(format!("New features ({} commits)", commit_count)),
                BumpType::Patch => reason_parts.push(format!("Bug fixes ({} commits)", commit_count)),
                BumpType::None => reason_parts.push(format!("{} changes (docs, chore)", commit_count)),
              }

              // Add API analysis if it increased the bump
              if api_bump > commit_bump {
                reason_parts.push("API breaking changes detected".to_string());
              }

              reason_parts.join("; ")
            };

            (combined_bump, reason)
          }
          Err(e) => {
            eprintln!("Warning: Failed to analyze commits for {}: {}", crate_name, e);
            (BumpType::Patch, "Changes detected (commit analysis failed)".to_string())
          }
        }
      } else {
        (BumpType::None, "No changes detected".to_string())
      };

      // Calculate next version
      let next_version = if bump_type == BumpType::None {
        pkg.version.to_string()
      } else {
        bump_type
          .apply(&pkg.version.to_string())
          .unwrap_or_else(|_| pkg.version.to_string())
      };

      // Update progress bar (if enabled)
      // Note: idx is always < bars.len() since we enumerate workspace_pkgs
      // and bars has exactly workspace_pkgs.len() elements
      if let Some(ref mp) = multi_progress {
        mp.inc(&bars[idx]);
      }

      CratePlan {
        name: pkg.name.to_string(),
        current_version: pkg.version.to_string(),
        next_version,
        bump_type,
        reason,
        has_changes,
      }
    })
    .collect();

  if show_progress {
    println!(); // Newline after progress bars
  }

  Ok(ReleasePlan {
    crates: crate_plans,
    publish_order,
  })
}

/// Run release plan command (CLI entry point)
pub fn run_release_plan(crate_name: Option<&str>, json: bool, all: bool) -> RailResult<()> {
  // Generate the plan with progress tracking
  let mut plan = generate_release_plan(true)?;

  // Filter to specific crate if requested
  if let Some(name) = crate_name {
    plan.crates.retain(|c| c.name == name);
    plan.publish_order.retain(|n| n == name);
  }

  // Filter to only changed crates unless --all
  if !all {
    plan = plan.only_changed();
  }

  // Output
  if json {
    println!("{}", plan.to_json()?);
  } else {
    println!("{}", plan.format_table());
  }

  Ok(())
}

/// Load workspace metadata using cargo_metadata
fn load_workspace_metadata() -> RailResult<cargo_metadata::Metadata> {
  let current_dir = env::current_dir().map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;

  Ok(MetadataCommand::new().current_dir(&current_dir).exec().map_err(|e| {
    anyhow::anyhow!(
      "Failed to load workspace metadata. Are you in a Cargo workspace?\n  Error: {}",
      e
    )
  })?)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_empty_plan() {
    let plan = ReleasePlan {
      crates: vec![],
      publish_order: vec![],
    };

    let table = plan.format_table();
    assert!(table.contains("No crates need to be released"));
  }

  #[test]
  fn test_plan_filtering() {
    let plan = ReleasePlan {
      crates: vec![
        CratePlan {
          name: "foo".to_string(),
          current_version: "0.1.0".to_string(),
          next_version: "0.2.0".to_string(),
          bump_type: BumpType::Minor,
          reason: "New features".to_string(),
          has_changes: true,
        },
        CratePlan {
          name: "bar".to_string(),
          current_version: "0.1.0".to_string(),
          next_version: "0.1.0".to_string(),
          bump_type: BumpType::None,
          reason: "No changes".to_string(),
          has_changes: false,
        },
      ],
      publish_order: vec!["foo".to_string(), "bar".to_string()],
    };

    let filtered = plan.only_changed();
    assert_eq!(filtered.crates.len(), 1);
    assert_eq!(filtered.crates[0].name, "foo");
    assert_eq!(filtered.publish_order, vec!["foo"]);
  }

  #[test]
  fn test_json_output() {
    let plan = ReleasePlan {
      crates: vec![CratePlan {
        name: "test".to_string(),
        current_version: "0.1.0".to_string(),
        next_version: "0.2.0".to_string(),
        bump_type: BumpType::Minor,
        reason: "Features added".to_string(),
        has_changes: true,
      }],
      publish_order: vec!["test".to_string()],
    };

    let json = plan.to_json().unwrap();
    assert!(json.contains("\"name\": \"test\""));
    assert!(json.contains("\"current_version\": \"0.1.0\""));
  }
}
