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
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES: usize = 120_000;
const RELEASE_REMOTE: &str = "origin";

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
  pub fn preflight_check(&self, plan: &ReleasePlan, skip_tag: bool) -> RailResult<Vec<String>> {
    let mut warnings = Vec::new();
    let git = self.ctx.git()?.git();

    if self.release_config.create_github_release && !self.release_config.push {
      return Err(RailError::with_help(
        "invalid release config: create_github_release requires push",
        "set [release].push = true so cargo-rail owns the pushed tag before creating a GitHub Release",
      ));
    }

    if self.release_config.create_github_release && !skip_tag {
      if !process::succeeds("gh", &["--version"], None) {
        return Err(RailError::with_help(
          "GitHub releases enabled but gh CLI was not found",
          "install gh from https://cli.github.com/ or set create_github_release = false",
        ));
      }

      if !process::succeeds("gh", &["auth", "status"], Some(self.ctx.workspace_root())) {
        return Err(RailError::with_help(
          "GitHub CLI is not authenticated",
          "run 'gh auth login' or provide GITHUB_TOKEN in CI",
        ));
      }

      for crate_plan in &plan.crates {
        if self.github_release_exists(&crate_plan.tag_name) {
          warnings.push(format!(
            "GitHub release '{}' already exists; cargo-rail will reuse it",
            crate_plan.tag_name
          ));
        }
      }
    }

    if self.release_config.push {
      if !git.has_remote(RELEASE_REMOTE)? {
        return Err(RailError::with_help(
          "release push enabled but remote 'origin' does not exist",
          "add an origin remote or set [release].push = false",
        ));
      }

      let branch = git.current_branch()?;
      let refspec = format!("HEAD:{}", branch);
      git.run_git(&["push", "--dry-run", RELEASE_REMOTE, &refspec])?;

      if !skip_tag {
        for crate_plan in &plan.crates {
          if self.remote_tag_exists(&crate_plan.tag_name)? {
            return Err(RailError::with_help(
              format!("remote tag '{}' already exists", crate_plan.tag_name),
              "choose a new version or inspect the existing release state before rerunning",
            ));
          }
        }
      }
    }

    // Check sign_tags prerequisites if enabled
    if self.release_config.sign_tags && !skip_tag {
      // Check if user has GPG/SSH key configured
      if !git.has_signing_configured() {
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
    let warnings = self.preflight_check(plan, skip_tag)?;
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
      self.validate_release_notes_size(crate_plan)?;

      progress!("  commit");
      self.commit_version_bump(crate_plan)?;

      if !skip_tag {
        progress!("  tag: {}", crate_plan.tag_name);
        self.create_tag(crate_plan)?;
      }
    }

    if self.release_config.push {
      progress!("  pushing release refs");
      self.push_release_refs(plan, skip_tag)?;
    }

    if self.release_config.create_github_release && !skip_tag {
      for crate_plan in &plan.crates {
        progress!("  github draft: {}", crate_plan.tag_name);
        self.create_github_release_draft(crate_plan)?;
      }
    }

    for (i, crate_plan) in plan.crates.iter().enumerate() {
      if !skip_publish && crate_plan.publish {
        progress!("  publishing {}...", crate_plan.name);
        self.publish_crate(crate_plan)?;

        if i + 1 < plan.crates.len() {
          let delay = self.release_config.publish_delay;
          progress!("  waiting {}s...", delay);
          thread::sleep(Duration::from_secs(delay));
        }
      } else if !crate_plan.publish {
        progress!("  skipped publish (publish = false) for {}", crate_plan.name);
      }
    }

    if self.release_config.create_github_release && !skip_tag {
      for crate_plan in &plan.crates {
        progress!("  github publish: {}", crate_plan.tag_name);
        self.publish_github_release(crate_plan)?;
      }
    }

    progress!("\nrelease complete");

    if !skip_tag && !self.release_config.push {
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

  fn push_release_refs(&self, plan: &ReleasePlan, skip_tag: bool) -> RailResult<()> {
    let git = self.ctx.git()?.git();
    let branch = git.current_branch()?;
    let head_refspec = format!("HEAD:{}", branch);
    let mut args = vec![
      "push".to_string(),
      "--atomic".to_string(),
      RELEASE_REMOTE.to_string(),
      head_refspec,
    ];

    if !skip_tag {
      for crate_plan in &plan.crates {
        args.push(format!("refs/tags/{}", crate_plan.tag_name));
      }
    }

    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    git.run_git(&borrowed)?;
    Ok(())
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

  /// Create a draft GitHub release targeting the exact pushed commit.
  fn create_github_release_draft(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    if self.github_release_exists(&plan.tag_name) {
      progress!("  github release already exists: {}", plan.tag_name);
      return Ok(());
    }

    let target = self.tag_target_commit(&plan.tag_name)?;
    let notes_file = self.write_release_notes_temp(plan)?;
    let output = process::run(
      "gh",
      &[
        "release",
        "create",
        &plan.tag_name,
        "--target",
        &target,
        "--title",
        &format!("{} v{}", plan.name, plan.new_version),
        "--notes-file",
        notes_file
          .to_str()
          .ok_or_else(|| RailError::message("release notes path is not valid UTF-8"))?,
        "--draft",
      ],
      Some(self.ctx.workspace_root()),
    )?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!(
        "gh release create failed for {}: {}",
        plan.tag_name,
        stderr.trim()
      )));
    }

    Ok(())
  }

  fn publish_github_release(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let output = process::run(
      "gh",
      &["release", "edit", &plan.tag_name, "--draft=false", "--latest"],
      Some(self.ctx.workspace_root()),
    )?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!(
        "gh release edit failed for {}: {}",
        plan.tag_name,
        stderr.trim()
      )));
    }

    Ok(())
  }

  fn github_release_exists(&self, tag_name: &str) -> bool {
    process::succeeds("gh", &["release", "view", tag_name], Some(self.ctx.workspace_root()))
  }

  fn remote_tag_exists(&self, tag_name: &str) -> RailResult<bool> {
    let output = self
      .ctx
      .git()?
      .git()
      .run_git(&["ls-remote", "--tags", RELEASE_REMOTE, tag_name])?;
    Ok(!output.stdout.is_empty())
  }

  fn tag_target_commit(&self, tag_name: &str) -> RailResult<String> {
    self.ctx.git()?.git().run_git_stdout(&["rev-list", "-n", "1", tag_name])
  }

  fn validate_release_notes_size(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    if !self.release_config.create_github_release {
      return Ok(());
    }

    let notes = self.release_notes(plan)?;
    if notes.len() > GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES {
      return Err(RailError::with_help(
        format!(
          "release notes for {} v{} are {} bytes, above the {} byte GitHub safety limit",
          plan.name,
          plan.new_version,
          notes.len(),
          GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES
        ),
        format!(
          "provide a shorter manual override at {}/v{}.md",
          self.release_config.release_notes_dir, plan.new_version
        ),
      ));
    }
    Ok(())
  }

  fn write_release_notes_temp(&self, plan: &CrateReleasePlan) -> RailResult<PathBuf> {
    let dir = self.ctx.workspace_root().join("target/cargo-rail/release-notes");
    fs::create_dir_all(&dir).map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
    let path = dir.join(format!("{}.md", sanitize_filename(&plan.tag_name)));
    fs::write(&path, self.release_notes(plan)?)
      .map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
    Ok(path)
  }

  fn release_notes(&self, plan: &CrateReleasePlan) -> RailResult<String> {
    if let Some(path) = self.release_notes_override_path(plan) {
      return fs::read_to_string(&path)
        .map_err(|e| RailError::message(format!("failed to read {}: {}", path.display(), e)));
    }

    if plan.changelog_path.exists() {
      let changelog = fs::read_to_string(&plan.changelog_path)
        .map_err(|e| RailError::message(format!("failed to read {}: {}", plan.changelog_path.display(), e)))?;
      if let Some(section) = extract_changelog_section(&changelog, &plan.new_version.to_string()) {
        return Ok(section);
      }
    }

    Ok(format!("Release {} v{}\n", plan.name, plan.new_version))
  }

  fn release_notes_override_path(&self, plan: &CrateReleasePlan) -> Option<PathBuf> {
    let dir = self.ctx.workspace_root().join(&self.release_config.release_notes_dir);
    let version_path = dir.join(format!("v{}.md", plan.new_version));
    if version_path.exists() {
      return Some(version_path);
    }

    let tag_path = dir.join(format!("{}.md", plan.tag_name));
    if tag_path.exists() {
      return Some(tag_path);
    }

    None
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

fn extract_changelog_section(changelog: &str, version: &str) -> Option<String> {
  let needle = format!("## [{}]", version);
  let mut section = String::new();
  let mut in_section = false;

  for line in changelog.lines() {
    if line.trim_start().starts_with("## ") {
      if in_section {
        break;
      }
      in_section = line.trim_start().starts_with(&needle);
    }

    if in_section {
      section.push_str(line);
      section.push('\n');
    }
  }

  if section.trim().is_empty() { None } else { Some(section) }
}

fn sanitize_filename(value: &str) -> String {
  value
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
        c
      } else {
        '-'
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_extract_changelog_section_returns_only_requested_version() {
    let changelog = r#"# Changelog

## [0.2.0] - 2026-06-01

### Features

- new API

## [0.1.0] - 2026-05-01

- old API
"#;

    let section = extract_changelog_section(changelog, "0.2.0").unwrap();
    assert!(section.contains("new API"));
    assert!(!section.contains("old API"));
  }
}
