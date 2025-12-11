//! Release execution (publishing to crates.io and GitHub)

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::release::changelog::ChangelogGenerator;
use crate::release::planner::{CrateReleasePlan, ReleasePlan};
use crate::release::version::VersionBumper;
use crate::workspace::WorkspaceContext;
use chrono::Local;
use std::fs;
use std::process::Command;
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

    // Issue #17: Check gh CLI availability if GitHub releases are enabled
    if self.release_config.create_github_release && !skip_tag {
      let check = Command::new("gh").args(["--version"]).output();
      if check.is_err() || !check.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        warnings.push(
          "GitHub releases enabled but 'gh' CLI not found. \
                    Install from https://cli.github.com/ or set create_github_release = false"
            .to_string(),
        );
      } else {
        // Check gh auth status
        let auth_check = Command::new("gh").args(["auth", "status"]).output();
        if auth_check.is_err() || !auth_check.as_ref().map(|o| o.status.success()).unwrap_or(false) {
          warnings.push("GitHub CLI not authenticated. Run 'gh auth login' first.".to_string());
        }
      }

      // Check for git remote
      let remote_check = Command::new("git")
        .current_dir(self.ctx.workspace_root())
        .args(["remote", "get-url", "origin"])
        .output();
      if remote_check.is_err() || !remote_check.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        warnings.push("No git remote 'origin' found. GitHub releases require a remote.".to_string());
      }
    }

    // Check sign_tags prerequisites if enabled
    if self.release_config.sign_tags && !skip_tag {
      // Check if user has GPG/SSH key configured
      let signing_check = Command::new("git")
        .current_dir(self.ctx.workspace_root())
        .args(["config", "--get", "user.signingkey"])
        .output();
      if signing_check.is_err() || !signing_check.as_ref().map(|o| o.status.success()).unwrap_or(false) {
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
      eprintln!("warning: {}", warning);
    }

    for (i, crate_plan) in plan.crates.iter().enumerate() {
      eprintln!("[{}/{}] {}", i + 1, plan.crates.len(), crate_plan.name);

      eprintln!(
        "  version: {} -> {}",
        crate_plan.current_version, crate_plan.new_version
      );
      self.bump_crate_version(crate_plan)?;

      if !crate_plan.affected_dependents.is_empty() {
        eprintln!("  updating {} dependents", crate_plan.affected_dependents.len());
        self.update_dependents(crate_plan)?;
      }

      eprintln!("  changelog");
      self.update_changelog(crate_plan)?;

      eprintln!("  commit");
      self.commit_version_bump(crate_plan)?;

      if !skip_tag {
        eprintln!("  tag: {}", crate_plan.tag_name);
        self.create_tag(crate_plan)?;
      }

      if !skip_publish && crate_plan.publish {
        eprintln!("  publishing...");
        self.publish_crate(crate_plan)?;

        if i + 1 < plan.crates.len() {
          let delay = self.release_config.publish_delay;
          eprintln!("  waiting {}s...", delay);
          thread::sleep(Duration::from_secs(delay));
        }
      } else if !crate_plan.publish {
        eprintln!("  skipped publish (publish = false)");
      }

      if self.release_config.create_github_release && !skip_tag {
        eprintln!("  github release");
        self.create_github_release(crate_plan)?;
      }
    }

    println!("\nrelease complete");

    if !skip_tag {
      println!("\nnext:");
      println!("  git push origin main");
      println!("  git push origin --tags");
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
    // This is necessary because editing Cargo.toml doesn't automatically update the lockfile
    let output = Command::new("cargo")
      .current_dir(self.ctx.workspace_root())
      .args(["update", "--workspace"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run cargo update: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!("cargo update failed: {}", stderr)));
    }

    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["add", "."])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git add: {}", e)))?;

    if !output.status.success() {
      return Err(RailError::message("git add failed"));
    }

    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["commit", "-m", &message])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git commit: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!("git commit failed: {}", stderr)));
    }

    Ok(())
  }

  /// Create git tag
  fn create_tag(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let mut cmd = Command::new("git");
    cmd.current_dir(self.ctx.workspace_root());

    // When sign_tags=false, explicitly disable signing to override user's git config
    // This ensures the user's tag.gpgsign=true doesn't interfere
    if self.release_config.sign_tags {
      cmd.args(["tag", "-s"]);
    } else {
      // Use -c to override any user git config that enables signing
      cmd.args(["-c", "tag.gpgsign=false", "tag", "-a"]);
    }

    cmd.args([
      &plan.tag_name,
      "-m",
      &format!("Release {} v{}", plan.name, plan.new_version),
    ]);

    let output = cmd
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git tag: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!("git tag failed: {}", stderr)));
    }

    Ok(())
  }

  /// Publish crate to crates.io
  fn publish_crate(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let crate_dir = plan
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    let output = Command::new("cargo")
      .current_dir(crate_dir)
      .args(["publish", "--allow-dirty"]) // dirty because we just committed
      .output()
      .map_err(|e| RailError::message(format!("Failed to run cargo publish: {}", e)))?;

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
    let check = Command::new("gh").args(["--version"]).output();

    if check.is_err() || !check.unwrap().status.success() {
      eprintln!("  skipped github release (gh CLI not found)");
      return Ok(());
    }

    let notes = if plan.changelog_path.exists() {
      fs::read_to_string(&plan.changelog_path)
        .unwrap_or_else(|_| format!("Release {} v{}", plan.name, plan.new_version))
    } else {
      format!("Release {} v{}", plan.name, plan.new_version)
    };

    let output = Command::new("gh")
      .current_dir(self.ctx.workspace_root())
      .args([
        "release",
        "create",
        &plan.tag_name,
        "--title",
        &format!("{} v{}", plan.name, plan.new_version),
        "--notes",
        &notes,
      ])
      .output()
      .map_err(|e| RailError::message(format!("gh release failed: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      eprintln!("  github release failed: {}", stderr.trim());
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

    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["tag", "--list", &pattern, "--sort=-version:refname"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git tag: {}", e)))?;

    if !output.status.success() {
      return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let tags: Vec<&str> = stdout.lines().collect();

    Ok(tags.first().map(|s| s.to_string()))
  }
}
