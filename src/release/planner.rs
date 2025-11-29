//! Release planning and dry-run analysis

use crate::config::{ChangelogRelativeTo, ReleaseConfig};
use crate::error::{RailError, RailResult};
use crate::release::version::BumpType;
use crate::workspace::WorkspaceContext;
use semver::Version;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// A plan for releasing one or more crates
#[derive(Debug, Clone, Serialize)]
pub struct ReleasePlan {
  /// Crates to release in dependency order
  pub crates: Vec<CrateReleasePlan>,
  /// Summary statistics
  pub summary: ReleaseSummary,
}

/// Release plan for a single crate
#[derive(Debug, Clone, Serialize)]
pub struct CrateReleasePlan {
  /// Crate name
  pub name: String,
  /// Current version
  pub current_version: Version,
  /// New version after bump
  pub new_version: Version,
  /// Path to Cargo.toml
  pub manifest_path: PathBuf,
  /// Path to CHANGELOG.md
  pub changelog_path: PathBuf,
  /// Git tag name for this release
  pub tag_name: String,
  /// Whether to publish to crates.io
  pub publish: bool,
  /// Whether to generate changelog
  pub generate_changelog: bool,
  /// Dependents that will need version updates
  pub affected_dependents: Vec<String>,
}

/// Summary statistics for a release plan
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseSummary {
  /// Total number of crates in the plan
  pub total_crates: usize,
  /// Number of crates that will be published
  pub crates_to_publish: usize,
  /// Number of crates that will be tagged
  pub crates_to_tag: usize,
}

/// Release planner
pub struct ReleasePlanner<'a> {
  /// Workspace context
  ctx: &'a WorkspaceContext,
  /// Release configuration
  release_config: &'a ReleaseConfig,
}

impl<'a> ReleasePlanner<'a> {
  /// Create a new release planner
  pub fn new(ctx: &'a WorkspaceContext, release_config: &'a ReleaseConfig) -> Self {
    Self { ctx, release_config }
  }

  /// Build a release plan
  ///
  /// # Arguments
  /// * `crate_names` - Specific crates to release, or None for all workspace members
  /// * `bump_type` - How to bump versions
  pub fn plan(&self, crate_names: Option<Vec<String>>, bump_type: &BumpType) -> RailResult<ReleasePlan> {
    // Determine which crates to release
    let target_crates = if let Some(names) = crate_names {
      names
    } else {
      self.ctx.graph.workspace_members()
    };

    // Get crates in dependency order
    let all_ordered = self.ctx.graph.publish_order()?;
    let ordered_targets: Vec<String> = all_ordered
      .into_iter()
      .filter(|name| target_crates.contains(name))
      .collect();

    // Build plan for each crate
    let mut crate_plans = Vec::new();
    let mut version_map: HashMap<String, Version> = HashMap::new();

    for crate_name in &ordered_targets {
      let plan = self.plan_crate(crate_name, bump_type, &version_map)?;
      version_map.insert(crate_name.clone(), plan.new_version.clone());
      crate_plans.push(plan);
    }

    // Build summary
    let crates_to_publish = crate_plans.iter().filter(|p| p.publish).count();
    let summary = ReleaseSummary {
      total_crates: crate_plans.len(),
      crates_to_publish,
      crates_to_tag: crate_plans.len(),
    };

    Ok(ReleasePlan {
      crates: crate_plans,
      summary,
    })
  }

  /// Plan release for a single crate
  fn plan_crate(
    &self,
    crate_name: &str,
    bump_type: &BumpType,
    _version_map: &HashMap<String, Version>,
  ) -> RailResult<CrateReleasePlan> {
    // Get crate metadata
    let metadata = self.ctx.cargo.metadata();
    let package = metadata
      .workspace_packages()
      .into_iter()
      .find(|pkg| pkg.name == crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    let manifest_path = package.manifest_path.clone().into_std_path_buf();

    // Get current version from cargo_metadata (already resolves workspace inheritance)
    let current_version = package.version.clone();

    // Calculate new version
    let new_version = bump_type.apply(&current_version);

    // Determine tag name
    let tag_name = self.format_tag(crate_name, &new_version);

    // Get per-crate config (if any)
    let crate_config = self.ctx.config.as_ref().and_then(|c| c.crates.get(crate_name));

    // Get changelog path - check per-crate config first, then fall back to global
    let changelog_relative_path = crate_config
      .and_then(|c| c.changelog.as_ref())
      .and_then(|ch| ch.path.as_ref())
      .map(|p| p.to_string_lossy().to_string())
      .unwrap_or_else(|| self.release_config.changelog_path.clone());

    // Resolve the path based on changelog_relative_to setting
    let changelog_path = match self.release_config.changelog_relative_to {
      ChangelogRelativeTo::Crate => {
        // Relative to crate directory (default)
        manifest_path
          .parent()
          .ok_or_else(|| RailError::message("Invalid manifest path"))?
          .join(&changelog_relative_path)
      }
      ChangelogRelativeTo::Workspace => {
        // Relative to workspace root
        self.ctx.workspace_root().join(&changelog_relative_path)
      }
    };

    // Check if changelog should be generated - per-crate config takes priority
    let generate_changelog = if let Some(changelog_cfg) = crate_config.and_then(|c| c.changelog.as_ref()) {
      // Per-crate config exists - use its skip value
      !changelog_cfg.skip
    } else {
      // Fall back to global skip_changelog_for list
      !self.release_config.skip_changelog_for.iter().any(|c| c == crate_name)
    };

    // Check if should publish - per-crate config takes priority, then Cargo.toml
    let publish_from_cargo = package.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true);
    let publish = crate_config
      .and_then(|c| c.release.as_ref())
      .map(|r| r.publish)
      .unwrap_or(publish_from_cargo);

    // Find affected dependents
    let affected_dependents = self.ctx.graph.transitive_dependents(crate_name)?;

    Ok(CrateReleasePlan {
      name: crate_name.to_string(),
      current_version,
      new_version,
      manifest_path,
      changelog_path,
      tag_name,
      publish,
      generate_changelog,
      affected_dependents,
    })
  }

  /// Format git tag name for a crate
  ///
  /// Supports placeholders:
  /// - {prefix} - the tag_prefix config value (default: "v")
  /// - {crate} - the crate name
  /// - {version} - the version number
  fn format_tag(&self, crate_name: &str, version: &Version) -> String {
    let workspace_members = self.ctx.graph.workspace_members();
    let is_single_crate = workspace_members.len() == 1;

    // For single-crate repos, use simple "{prefix}{version}" format
    // For monorepos, use tag_format with all placeholders
    if is_single_crate {
      format!("{}{}", self.release_config.tag_prefix, version)
    } else {
      // Apply all placeholders including {prefix}
      self
        .release_config
        .tag_format
        .replace("{prefix}", &self.release_config.tag_prefix)
        .replace("{crate}", crate_name)
        .replace("{version}", &version.to_string())
    }
  }
}

impl ReleasePlan {
  /// Format summary for display
  ///
  /// # Arguments
  /// * `skip_publish` - If true, show publishing as skipped for all crates
  /// * `skip_tag` - If true, show tagging as skipped for all crates
  pub fn format_summary_with_flags(&self, skip_publish: bool, skip_tag: bool) -> String {
    let mut output = String::new();

    output.push_str("📦 Release Plan\n\n");

    for (i, crate_plan) in self.crates.iter().enumerate() {
      output.push_str(&format!("{}. {}\n", i + 1, crate_plan.name));
      output.push_str(&format!(
        "   Version: {} → {}\n",
        crate_plan.current_version, crate_plan.new_version
      ));

      // Show tag status
      let tag_status = if skip_tag {
        "✗ (--skip-tag)"
      } else {
        &crate_plan.tag_name
      };
      output.push_str(&format!("   Tag: {}\n", tag_status));

      // Show publish status
      let publish_status = if skip_publish {
        "✗ (--skip-publish)".to_string()
      } else if crate_plan.publish {
        "✓".to_string()
      } else {
        "✗ (publish = false)".to_string()
      };
      output.push_str(&format!("   Publish: {}\n", publish_status));

      if !crate_plan.affected_dependents.is_empty() {
        output.push_str(&format!("   Affects: {}\n", crate_plan.affected_dependents.join(", ")));
      }
      output.push('\n');
    }

    // Adjust summary counts based on flags
    let effective_publish_count = if skip_publish {
      0
    } else {
      self.summary.crates_to_publish
    };
    let effective_tag_count = if skip_tag { 0 } else { self.summary.crates_to_tag };

    output.push_str(&format!(
      "Summary: {} crate(s), {} to publish, {} tag(s)\n",
      self.summary.total_crates, effective_publish_count, effective_tag_count
    ));

    output
  }

  /// Format summary for display (default: no skip flags)
  pub fn format_summary(&self) -> String {
    self.format_summary_with_flags(false, false)
  }
}
