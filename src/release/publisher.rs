//! Release execution and publishing to crates.io and forge releases.

use crate::config::{ReleaseConfig, ReleaseForgeConfig};
use crate::error::{RailError, RailResult};
use crate::release::changelog::detect_github_repo;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseForge {
  Github,
  Gitlab,
}

impl ReleaseForge {
  fn name(self) -> &'static str {
    match self {
      Self::Github => "GitHub",
      Self::Gitlab => "GitLab",
    }
  }

  fn binary(self) -> &'static str {
    match self {
      Self::Github => "gh",
      Self::Gitlab => "glab",
    }
  }
}

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
        "set [release].push = true so cargo-rail owns the pushed tag before creating a forge release",
      ));
    }

    if self.release_config.create_github_release && !skip_tag {
      let forge = self.detect_release_forge()?;
      let binary = forge.binary();
      if !process::succeeds(binary, &["--version"], None) {
        return Err(RailError::with_help(
          format!("{} releases enabled but {} CLI was not found", forge.name(), binary),
          format!("install {} or set create_github_release = false", binary),
        ));
      }

      if forge == ReleaseForge::Github && !process::succeeds("gh", &["auth", "status"], Some(self.ctx.workspace_root()))
      {
        return Err(RailError::with_help(
          "GitHub CLI is not authenticated",
          "run 'gh auth login' or provide GITHUB_TOKEN in CI",
        ));
      }

      for crate_plan in &plan.crates {
        if self.forge_release_exists(forge, &crate_plan.tag_name) {
          warnings.push(format!(
            "{} release '{}' already exists; cargo-rail will reuse it",
            forge.name(),
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
      git.run_git(&["push", "--dry-run", "--no-verify", RELEASE_REMOTE, &refspec])?;

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

    let mut consumed_change_files = false;
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
      self.validate_release_notes_size(crate_plan, skip_tag)?;
      if !consumed_change_files {
        self.consume_change_files(plan)?;
        consumed_change_files = true;
      }

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
      let forge = self.detect_release_forge()?;
      for crate_plan in &plan.crates {
        progress!("  {} release: {}", forge.name().to_lowercase(), crate_plan.tag_name);
        self.create_forge_release(forge, crate_plan)?;
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
      let forge = self.detect_release_forge()?;
      for crate_plan in &plan.crates {
        self.publish_forge_release(forge, crate_plan)?;
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

  /// Prepare a release pull request: mutations only, no tags or publish.
  pub fn execute_pr(&self, plan: &ReleasePlan) -> RailResult<()> {
    self.preflight_pr()?;
    let branch = release_branch_name(plan)?;
    let git = self.ctx.git()?.git();
    git.run_git(&["checkout", "-B", &branch])?;

    let mut consumed_change_files = false;
    for crate_plan in &plan.crates {
      progress!(
        "  version: {} -> {}",
        crate_plan.current_version,
        crate_plan.new_version
      );
      self.bump_crate_version(crate_plan)?;

      if !crate_plan.affected_dependents.is_empty() {
        self.update_dependents(crate_plan)?;
      }

      self.update_changelog(crate_plan)?;
      if !consumed_change_files {
        self.consume_change_files(plan)?;
        consumed_change_files = true;
      }
      self.update_lockfile_for_crate(&crate_plan.name)?;
    }

    git.stage_all()?;
    git.commit(&format!("chore(release): prepare {}", branch))?;
    git.run_git(&["push", "-u", RELEASE_REMOTE, &branch])?;
    self.open_release_pr(plan, &branch)?;
    progress!("release PR ready: {}", branch);
    Ok(())
  }

  /// Finalize an already-merged release PR: tags, push, publish, forge releases.
  pub fn execute_finalize(&self, plan: &ReleasePlan, skip_publish: bool, skip_tag: bool) -> RailResult<()> {
    let warnings = self.preflight_check(plan, skip_tag)?;
    for warning in &warnings {
      warn!("{}", warning);
    }

    for crate_plan in &plan.crates {
      self.validate_release_notes_size(crate_plan, skip_tag)?;
    }

    if !skip_tag {
      for crate_plan in &plan.crates {
        progress!("  tag: {}", crate_plan.tag_name);
        self.create_tag(crate_plan)?;
      }
    }

    if self.release_config.push {
      progress!("  pushing release refs");
      self.push_release_refs(plan, skip_tag)?;
    }

    if self.release_config.create_github_release && !skip_tag {
      let forge = self.detect_release_forge()?;
      for crate_plan in &plan.crates {
        progress!("  {} release: {}", forge.name().to_lowercase(), crate_plan.tag_name);
        self.create_forge_release(forge, crate_plan)?;
      }
    }

    for (i, crate_plan) in plan.crates.iter().enumerate() {
      if !skip_publish && crate_plan.publish {
        progress!("  publishing {}...", crate_plan.name);
        self.publish_crate(crate_plan)?;
        if i + 1 < plan.crates.len() {
          thread::sleep(Duration::from_secs(self.release_config.publish_delay));
        }
      }
    }

    if self.release_config.create_github_release && !skip_tag {
      let forge = self.detect_release_forge()?;
      for crate_plan in &plan.crates {
        self.publish_forge_release(forge, crate_plan)?;
      }
    }

    progress!("\nrelease finalize complete");
    Ok(())
  }

  fn preflight_pr(&self) -> RailResult<()> {
    let git = self.ctx.git()?.git();
    if !git.has_remote(RELEASE_REMOTE)? {
      return Err(RailError::with_help(
        "release PR mode requires remote 'origin'",
        "add an origin remote before running 'cargo rail release run --pr'",
      ));
    }
    if !process::succeeds("gh", &["--version"], None) {
      return Err(RailError::with_help(
        "release PR mode requires gh CLI",
        "install gh from https://cli.github.com/ or run the release without --pr",
      ));
    }
    Ok(())
  }

  fn open_release_pr(&self, plan: &ReleasePlan, branch: &str) -> RailResult<()> {
    let body_path = self.write_release_pr_body(plan, branch)?;
    let output = process::run(
      "gh",
      &[
        "pr",
        "create",
        "--title",
        &format!("Release {}", branch.trim_start_matches("rail/release-")),
        "--body-file",
        body_path
          .to_str()
          .ok_or_else(|| RailError::message("release PR body path is not valid UTF-8"))?,
        "--head",
        branch,
      ],
      Some(self.ctx.workspace_root()),
    )?;
    if !output.status.success() {
      return Err(RailError::message(format!(
        "gh pr create failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      )));
    }
    Ok(())
  }

  fn write_release_pr_body(&self, plan: &ReleasePlan, branch: &str) -> RailResult<PathBuf> {
    let dir = self.ctx.workspace_root().join("target/cargo-rail/release-pr");
    fs::create_dir_all(&dir).map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
    let path = dir.join(format!("{}.md", sanitize_filename(branch)));
    fs::write(&path, release_pr_body(plan))
      .map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
    Ok(path)
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

  fn consume_change_files(&self, plan: &ReleasePlan) -> RailResult<()> {
    for path in &plan.change_files_to_delete {
      if path.exists() {
        fs::remove_file(path)
          .map_err(|e| RailError::message(format!("failed to remove change file {}: {}", path.display(), e)))?;
      }
    }
    Ok(())
  }

  /// Update or create CHANGELOG.md
  fn update_changelog(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    if !plan.generate_changelog {
      return Ok(());
    }

    let github_repo = detect_github_repo(self.ctx.workspace_root());
    let new_entries = plan.changelog_body.as_str();

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
    updated.push_str(&self.format_version_header(plan, plan.previous_tag.as_deref(), &date, github_repo.as_ref()));
    updated.push_str(new_entries);
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

  fn create_forge_release(&self, forge: ReleaseForge, plan: &CrateReleasePlan) -> RailResult<()> {
    if self.forge_release_exists(forge, &plan.tag_name) {
      progress!(
        "  {} release already exists: {}",
        forge.name().to_lowercase(),
        plan.tag_name
      );
      return Ok(());
    }
    match forge {
      ReleaseForge::Github => self.create_github_release_draft(plan),
      ReleaseForge::Gitlab => self.create_gitlab_release(plan),
    }
  }

  /// Create a draft GitHub release targeting the exact pushed commit.
  fn create_github_release_draft(&self, plan: &CrateReleasePlan) -> RailResult<()> {
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

  fn create_gitlab_release(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let notes_file = self.write_release_notes_temp(plan)?;
    let args = gitlab_release_create_args(
      &plan.tag_name,
      &format!("{} v{}", plan.name, plan.new_version),
      notes_file
        .to_str()
        .ok_or_else(|| RailError::message("release notes path is not valid UTF-8"))?,
    );
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = process::run("glab", &borrowed, Some(self.ctx.workspace_root()))?;
    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!(
        "glab release create failed for {}: {}",
        plan.tag_name,
        stderr.trim()
      )));
    }
    Ok(())
  }

  fn publish_forge_release(&self, forge: ReleaseForge, plan: &CrateReleasePlan) -> RailResult<()> {
    match forge {
      ReleaseForge::Github => self.publish_github_release(plan),
      ReleaseForge::Gitlab => Ok(()),
    }
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

  fn forge_release_exists(&self, forge: ReleaseForge, tag_name: &str) -> bool {
    match forge {
      ReleaseForge::Github => process::succeeds("gh", &["release", "view", tag_name], Some(self.ctx.workspace_root())),
      ReleaseForge::Gitlab => {
        process::succeeds("glab", &["release", "view", tag_name], Some(self.ctx.workspace_root()))
      }
    }
  }

  fn detect_release_forge(&self) -> RailResult<ReleaseForge> {
    match self.release_config.forge {
      ReleaseForgeConfig::Github => return Ok(ReleaseForge::Github),
      ReleaseForgeConfig::Gitlab => return Ok(ReleaseForge::Gitlab),
      ReleaseForgeConfig::Auto => {}
    }

    let output = process::run(
      "git",
      &["config", "--get", "remote.origin.url"],
      Some(self.ctx.workspace_root()),
    )?;
    let remote = String::from_utf8_lossy(&output.stdout);
    detect_release_forge_from_remote(remote.trim()).ok_or_else(|| {
      RailError::with_help(
        "could not detect release forge from origin remote",
        "set [release].forge = \"github\" or \"gitlab\"; Gitea release creation is not supported",
      )
    })
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

  fn validate_release_notes_size(&self, plan: &CrateReleasePlan, skip_tag: bool) -> RailResult<()> {
    if !self.release_config.create_github_release || skip_tag || self.detect_release_forge()? != ReleaseForge::Github {
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

fn release_branch_name(plan: &ReleasePlan) -> RailResult<String> {
  let json = serde_json::to_string(plan)
    .map_err(|e| RailError::message(format!("failed to serialize release plan for branch hash: {}", e)))?;
  Ok(format!("rail/release-{}", short_hash(&json)))
}

fn detect_release_forge_from_remote(remote: &str) -> Option<ReleaseForge> {
  let lower = remote.to_ascii_lowercase();
  if lower.contains("github.com") {
    Some(ReleaseForge::Github)
  } else if lower.contains("gitlab.com") {
    Some(ReleaseForge::Gitlab)
  } else {
    None
  }
}

fn gitlab_release_create_args(tag: &str, title: &str, notes_file: &str) -> Vec<String> {
  vec![
    "release".to_string(),
    "create".to_string(),
    tag.to_string(),
    "--name".to_string(),
    title.to_string(),
    "--notes-file".to_string(),
    notes_file.to_string(),
  ]
}

fn short_hash(value: &str) -> String {
  let mut hash = 0xcbf29ce484222325_u64;
  for byte in value.as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(0x100000001b3);
  }
  format!("{:08x}", hash & 0xffff_ffff)
}

fn release_pr_body(plan: &ReleasePlan) -> String {
  let mut out = plan.format_summary_with_flags(true, true);
  out.push_str("\n## Changelog Bodies\n\n");
  for crate_plan in &plan.crates {
    out.push_str(&format!("### {} v{}\n\n", crate_plan.name, crate_plan.new_version));
    if crate_plan.changelog_body.trim().is_empty() {
      out.push_str("_No generated changelog entries._\n\n");
    } else {
      out.push_str(crate_plan.changelog_body.trim());
      out.push_str("\n\n");
    }
  }
  out
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

  #[test]
  fn release_branch_name_is_stable() {
    let plan = ReleasePlan {
      plan_contract_version: 2,
      canonical_crate_order: Vec::new(),
      crates: Vec::new(),
      summary: crate::release::planner::ReleaseSummary {
        total_crates: 0,
        crates_to_publish: 0,
        crates_to_tag: 0,
      },
      change_files_to_delete: Vec::new(),
      skipped: Vec::new(),
    };
    assert_eq!(release_branch_name(&plan).unwrap(), release_branch_name(&plan).unwrap());
    assert!(release_branch_name(&plan).unwrap().starts_with("rail/release-"));
  }

  #[test]
  fn detects_release_forge_from_common_remotes() {
    assert_eq!(
      detect_release_forge_from_remote("git@github.com:org/repo.git"),
      Some(ReleaseForge::Github)
    );
    assert_eq!(
      detect_release_forge_from_remote("https://gitlab.com/org/repo.git"),
      Some(ReleaseForge::Gitlab)
    );
    assert_eq!(
      detect_release_forge_from_remote("https://git.example/org/repo.git"),
      None
    );
  }

  #[test]
  fn gitlab_release_create_args_match_glab_cli() {
    assert_eq!(
      gitlab_release_create_args("v1.0.0", "crate v1.0.0", "/tmp/notes.md"),
      vec![
        "release",
        "create",
        "v1.0.0",
        "--name",
        "crate v1.0.0",
        "--notes-file",
        "/tmp/notes.md"
      ]
    );
  }
}
