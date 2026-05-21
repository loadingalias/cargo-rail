//! Release execution and publishing to crates.io and GitHub (for now)

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::release::changelog::ChangelogGenerator;
use crate::release::planner::{CrateReleasePlan, ReleasePlan};
use crate::release::process;
use crate::release::version::VersionBumper;
use crate::workspace::WorkspaceContext;
use crate::{progress, warn};
use chrono::Local;
use std::fs;
use std::thread;
use std::time::Duration;

/// Release publisher
pub struct ReleasePublisher<'a> {
  /// Workspace context
  ctx: &'a WorkspaceContext,
  /// Release configuration
  release_config: &'a ReleaseConfig,
}

impl<'a> ReleasePublisher<'a> {
  /// Create a new release publisher
  pub fn new(ctx: &'a WorkspaceContext, release_config: &'a ReleaseConfig) -> Self {
    Self { ctx, release_config }
  }

  /// Pre-flight validation: check all prerequisites before starting release
  ///
  /// This catches issues early rather than failing mid-release.
  pub fn preflight_check(&self, skip_tag: bool) -> RailResult<Vec<String>> {
    let mut warnings = Vec::new();

    // Check gh CLI availability if GitHub releases are enabled
    if self.release_config.create_github_release && !skip_tag {
      if !process::succeeds("gh", &["--version"], None) {
        warnings.push(
          "GitHub releases enabled but 'gh' CLI not found. \
                    Install from https://cli.github.com/ or set create_github_release = false"
            .to_string(),
        );
      } else {
        // Check gh auth status
        if !process::succeeds("gh", &["auth", "status"], None) {
          warnings.push("GitHub CLI not authenticated. Run 'gh auth login' first.".to_string());
        }
      }

      // Check for git remote
      if !self.ctx.git()?.git().has_remote("origin")? {
        warnings.push("No git remote 'origin' found. GitHub releases require a remote.".to_string());
      }
    }

    // Check sign_tags prerequisites if enabled
    if self.release_config.sign_tags && !skip_tag {
      // Check if user has GPG/SSH key configured
      if !self.ctx.git()?.git().has_signing_configured() {
        warnings.push(
          "Tag signing enabled but no signing key configured. \
                    Run 'git config user.signingkey <KEY_ID>'"
            .to_string(),
        );
      }
    }

    Ok(warnings)
  }

  /// Execute a release plan
  pub fn execute(&self, plan: &ReleasePlan, skip_publish: bool, skip_tag: bool) -> RailResult<()> {
    // Run pre-flight checks
    let warnings = self.preflight_check(skip_tag)?;
    for warning in &warnings {
      warn!("{}", warning);
    }

    for (i, crate_plan) in plan.crates.iter().enumerate() {
      progress!("[{}/{}] {}", i + 1, plan.crates.len(), crate_plan.name);

      progress!(
        "  version: {} -> {}",
        crate_plan.current_version,
        crate_plan.new_version
      );
      self.bump_crate_version(crate_plan)?;

      if !crate_plan.affected_dependents.is_empty() {
        progress!("  updating {} dependents", crate_plan.affected_dependents.len());
        self.update_dependents(crate_plan)?;
      }

      progress!("  changelog");
      self.update_changelog(crate_plan)?;

      progress!("  commit");
      self.commit_version_bump(crate_plan)?;

      if !skip_tag {
        progress!("  tag: {}", crate_plan.tag_name);
        self.create_tag(crate_plan)?;
      }

      if !skip_publish && crate_plan.publish {
        progress!("  publishing...");
        self.publish_crate(crate_plan)?;

        if i + 1 < plan.crates.len() {
          let delay = self.release_config.publish_delay;
          progress!("  waiting {}s...", delay);
          thread::sleep(Duration::from_secs(delay));
        }
      } else if !crate_plan.publish {
        progress!("  skipped publish (publish = false)");
      }

      if self.release_config.create_github_release && !skip_tag {
        progress!("  github release");
        self.create_github_release(crate_plan)?;
      }
    }

    progress!("\nrelease complete");

    if !skip_tag {
      let branch = self.ctx.git()?.current_branch().unwrap_or_else(|_| "main".to_string());
      progress!("\nnext:");
      progress!("  git push origin {}", branch);
      progress!("  git push origin --tags");
    }

    Ok(())
  }

  /// Bump version in Cargo.toml
  fn bump_crate_version(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    use crate::release::version::BumpType;
    let bump = BumpType::Exact(plan.new_version.clone());
    VersionBumper::bump_version(&plan.manifest_path, bump)?;
    Ok(())
  }

  /// Update dependent crates to use new version
  fn update_dependents(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    // Update [workspace.dependencies] in root Cargo.toml
    let root_manifest = self.ctx.workspace_root().join("Cargo.toml");
    VersionBumper::update_workspace_dependency(&root_manifest, &plan.name, &plan.new_version)?;

    // Update dependent crate manifests
    for dependent_name in &plan.affected_dependents {
      if let Some(pkg) = self.ctx.cargo.get_package(dependent_name) {
        let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
        VersionBumper::update_dependency_version(&manifest_path, &plan.name, &plan.new_version)?;
      }
    }

    Ok(())
  }

  /// Update Cargo.lock for a specific crate only
  ///
  /// Uses targeted `cargo update --package` to avoid upgrading external dependencies.
  /// This is safer than `cargo update --workspace` which can inadvertently upgrade
  /// pinned external dependencies during a release.
  fn update_lockfile_for_crate(&self, crate_name: &str) -> RailResult<()> {
    let output = process::run(
      "cargo",
      &["update", "--package", crate_name],
      Some(self.ctx.workspace_root()),
    )?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!(
        "cargo update --package {} failed: {}",
        crate_name, stderr
      )));
    }

    Ok(())
  }

  /// Update or create CHANGELOG.md
  fn update_changelog(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    if !plan.generate_changelog {
      return Ok(());
    }

    // Find previous tag for this crate
    let previous_tag = self.find_previous_tag(plan)?;

    // Get crate directory for path filtering
    let crate_dir = plan
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    // Generate changelog
    let generator = ChangelogGenerator::new(self.ctx.workspace_root());
    let github_repo = generator.github_repo().cloned();
    let new_entries = generator.generate(previous_tag.as_deref(), "HEAD", Some(&[crate_dir]))?;

    // Read existing changelog or create new
    let existing = if plan.changelog_path.exists() {
      fs::read_to_string(&plan.changelog_path).unwrap_or_default()
    } else {
      format!(
        "# Changelog\n\nAll notable changes to {} will be documented in this file.\n\n",
        plan.name
      )
    };

    // Prepend new version section
    let mut updated = String::new();
    let lines: Vec<&str> = existing.lines().collect();

    // Add header
    if let Some(header) = lines.first() {
      updated.push_str(header);
      updated.push_str("\n\n");
    }

    // Add new version with today's date
    let date = self.get_current_date();
    updated.push_str(&self.format_version_header(plan, previous_tag.as_deref(), &date, github_repo.as_ref()));
    updated.push_str(&new_entries);
    updated.push('\n');

    if new_entries.trim().is_empty() {
      if self.release_config.require_changelog_entries {
        return Err(RailError::message(format!(
          "no changelog entries for {} (enable commits or disable changelog)",
          plan.name
        )));
      }
      return Ok(());
    }

    // Add rest of existing changelog
    if lines.len() > 1 {
      for line in &lines[1..] {
        updated.push_str(line);
        updated.push('\n');
      }
    }

    // Auto-create parent directories if they don't exist
    if let Some(parent) = plan.changelog_path.parent()
      && !parent.exists()
    {
      fs::create_dir_all(parent)
        .map_err(|e| RailError::message(format!("failed to create directory {}: {}", parent.display(), e)))?;
    }

    fs::write(&plan.changelog_path, updated)
      .map_err(|e| RailError::message(format!("failed to write {}: {}", plan.changelog_path.display(), e)))?;

    Ok(())
  }

  /// Commit version bump and changelog
  fn commit_version_bump(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let message = format!("chore(release): {} v{}", plan.name, plan.new_version);

    // Update Cargo.lock to reflect the new version
    // Use targeted update to only update this crate, not external dependencies
    self.update_lockfile_for_crate(&plan.name)?;

    // Stage all changes and commit
    self.ctx.git()?.git().stage_all()?;
    self.ctx.git()?.git().commit(&message)?;

    Ok(())
  }

  /// Create git tag
  fn create_tag(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let message = format!("Release {} v{}", plan.name, plan.new_version);
    self
      .ctx
      .git()?
      .git()
      .create_tag(&plan.tag_name, Some(&message), self.release_config.sign_tags)
  }

  /// Publish crate to crates.io
  fn publish_crate(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let crate_dir = plan
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    let output = process::run("cargo", &["publish", "--allow-dirty"], Some(crate_dir))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!(
        "cargo publish failed for {}: {}",
        plan.name, stderr
      )));
    }

    Ok(())
  }

  /// Create GitHub release using gh CLI
  fn create_github_release(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    if !process::succeeds("gh", &["--version"], None) {
      progress!("  skipped github release (gh CLI not found)");
      return Ok(());
    }

    let notes = if plan.changelog_path.exists() {
      fs::read_to_string(&plan.changelog_path)
        .unwrap_or_else(|_| format!("Release {} v{}", plan.name, plan.new_version))
    } else {
      format!("Release {} v{}", plan.name, plan.new_version)
    };

    let output = process::run(
      "gh",
      &[
        "release",
        "create",
        &plan.tag_name,
        "--title",
        &format!("{} v{}", plan.name, plan.new_version),
        "--notes",
        &notes,
      ],
      Some(self.ctx.workspace_root()),
    )?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      progress!("  github release failed: {}", stderr.trim());
    }

    Ok(())
  }

  /// Get current date in YYYY-MM-DD format
  fn get_current_date(&self) -> String {
    Local::now().format("%Y-%m-%d").to_string()
  }

  fn format_version_header(
    &self,
    plan: &CrateReleasePlan,
    previous_tag: Option<&str>,
    date: &str,
    github_repo: Option<&(String, String)>,
  ) -> String {
    if let Some((org, repo)) = github_repo {
      let url = if let Some(prev) = previous_tag {
        format!(
          "https://github.com/{}/{}/compare/{}...{}",
          org, repo, prev, plan.tag_name
        )
      } else {
        format!("https://github.com/{}/{}/releases/tag/{}", org, repo, plan.tag_name)
      };

      return format!("## [{}]({}) - {}\n\n", plan.new_version, url, date);
    }

    format!("## [{}] - {}\n\n", plan.new_version, date)
  }

  /// Find previous tag for a crate
  fn find_previous_tag(&self, plan: &CrateReleasePlan) -> RailResult<Option<String>> {
    let workspace_members = self.ctx.graph.workspace_members();
    let is_single_crate = workspace_members.len() == 1;

    let pattern = if is_single_crate {
      format!("{}*", self.release_config.tag_prefix)
    } else {
      self
        .release_config
        .tag_format
        .replace("{crate}", &plan.name)
        .replace("{version}", "*")
    };

    self.ctx.git()?.git().find_latest_tag(&pattern)
  }
}
