//! Pre-release validation checks

use crate::config::{ChangelogRelativeTo, ReleaseConfig, SemverCheckPolicy};
use crate::error::{RailError, RailResult};
use crate::release::planner::ReleasePlan;
use crate::release::process;
use crate::release::semver_checks;
use crate::workspace::WorkspaceContext;
use std::fs;
use std::path::PathBuf;

/// Result of a single validation check
#[derive(Debug, Clone)]
pub struct ValidationResult {
  /// Name of the check
  pub check_name: String,
  /// Whether the check passed
  pub passed: bool,
  /// Details about what was validated
  pub details: Option<String>,
  /// Error message if failed
  pub error: Option<String>,
}

impl ValidationResult {
  fn passed(name: impl Into<String>, details: impl Into<String>) -> Self {
    Self {
      check_name: name.into(),
      passed: true,
      details: Some(details.into()),
      error: None,
    }
  }

  fn failed(name: impl Into<String>, error: impl Into<String>) -> Self {
    Self {
      check_name: name.into(),
      passed: false,
      details: None,
      error: Some(error.into()),
    }
  }
}

/// Pre-release validator
pub struct ReleaseValidator<'a> {
  /// Workspace context
  ctx: &'a WorkspaceContext,
}

impl<'a> ReleaseValidator<'a> {
  /// Create a new release validator
  pub fn new(ctx: &'a WorkspaceContext) -> Self {
    Self { ctx }
  }

  /// Validate release readiness for crate(s)
  pub fn validate(&self, crate_names: &[String], require_clean: bool) -> RailResult<()> {
    // 1. Check working directory is clean
    if require_clean {
      self.check_clean_working_directory()?;
    }

    // 2. Validate crates exist in workspace
    let workspace_members = self.ctx.graph.workspace_members();
    for crate_name in crate_names {
      if !workspace_members.contains(crate_name) {
        return Err(RailError::with_help(
          format!("Crate '{}' not found in workspace", crate_name),
          format!("Available crates: {}", workspace_members.join(", ")),
        ));
      }
    }

    // 3. Check for path dependencies
    for crate_name in crate_names {
      self.check_path_dependencies(crate_name)?;
    }

    Ok(())
  }

  /// Validate git branch state for release
  ///
  /// Checks:
  /// - Detached HEAD: hard error (cannot release without a branch)
  /// - Non-default branch: error unless `allow_non_default` is true, then returns warning
  ///
  /// Returns `Some(warning)` if releasing from non-default branch with `allow_non_default=true`.
  /// Returns `None` if on default branch or no default branch can be determined.
  pub fn validate_branch(&self, allow_non_default: bool) -> RailResult<Option<String>> {
    let git = self.ctx.git()?;

    // Hard error: detached HEAD
    if git.is_detached_head()? {
      return Err(RailError::with_help(
        "Cannot release from detached HEAD",
        "Checkout a branch first: git checkout <branch-name>",
      ));
    }

    let current = git.current_branch()?;

    // Check if on default branch
    if let Some(default) = git.default_branch()? {
      if current != default && !allow_non_default {
        return Err(RailError::with_help(
          format!("Releasing from '{}', not default branch '{}'", current, default),
          format!("Pass --yes to confirm, or checkout {}", default),
        ));
      }
      if current != default {
        // Return warning for display
        return Ok(Some(format!(
          "warning: releasing from '{}', not default branch '{}'",
          current, default
        )));
      }
    }

    Ok(None) // No warnings
  }

  /// Check if working directory is clean (no uncommitted changes)
  fn check_clean_working_directory(&self) -> RailResult<()> {
    if !self.ctx.changed_source_paths()?.is_empty() {
      return Err(RailError::with_help(
        "Working directory has uncommitted changes",
        "Commit or stash your changes before releasing, or set require_clean = false in [release] section of rail.toml",
      ));
    }

    Ok(())
  }

  /// Validate that crate can be published to crates.io
  pub fn validate_publishable(&self, crate_name: &str) -> RailResult<()> {
    let package = self
      .ctx
      .cargo
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    // Check if publish = false in Cargo.toml
    if !crate::workspace::CargoState::is_package_publishable(package) {
      return Err(RailError::with_help(
        format!("Crate '{}' has publish = false in Cargo.toml", crate_name),
        "Remove 'publish = false' or exclude this crate from the release",
      ));
    }

    Ok(())
  }

  /// Validate preconditions for applying a release plan.
  pub fn validate_apply_preconditions(
    &self,
    plan: &ReleasePlan,
    skip_publish: bool,
    skip_tag: bool,
    require_clean: bool,
    require_release_notes: bool,
  ) -> RailResult<()> {
    if require_clean {
      self.check_clean_working_directory()?;
    }

    if !skip_tag {
      for crate_plan in &plan.crates {
        if self.ctx.git()?.git().tag_exists(&crate_plan.tag_name)? {
          return Err(RailError::with_help(
            format!("tag '{}' already exists", crate_plan.tag_name),
            "regenerate plan with a new version or delete the conflicting tag".to_string(),
          ));
        }
      }
    }

    if !skip_publish && plan.crates.iter().any(|crate_plan| crate_plan.publish) {
      let output = process::run(
        "cargo",
        &["search", "serde", "--limit", "1"],
        Some(self.ctx.workspace_root()),
      )?;
      if !output.status.success() {
        return Err(RailError::with_help(
          "crates.io precondition check failed",
          "verify network access and cargo credentials before publishing".to_string(),
        ));
      }

      for crate_plan in plan.crates.iter().filter(|crate_plan| crate_plan.publish) {
        if self.crates_io_version_exists(&crate_plan.name, &crate_plan.new_version.to_string())? {
          return Err(RailError::with_help(
            format!(
              "{} v{} already exists on crates.io",
              crate_plan.name, crate_plan.new_version
            ),
            "choose a new version before running release apply",
          ));
        }
      }
    }

    if require_release_notes {
      self.validate_release_notes(plan)?;
    }

    Ok(())
  }

  fn crates_io_version_exists(&self, crate_name: &str, version: &str) -> RailResult<bool> {
    let output = process::run(
      "cargo",
      &["search", crate_name, "--limit", "5"],
      Some(self.ctx.workspace_root()),
    )?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::with_help(
        format!("failed to query crates.io for {}", crate_name),
        stderr.trim().to_string(),
      ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let quoted_version = format!("\"{}\"", version);
    Ok(stdout.lines().any(|line| {
      let Some((name, rest)) = line.split_once(" = ") else {
        return false;
      };
      name.trim() == crate_name && rest.trim_start().starts_with(&quoted_version)
    }))
  }

  /// Ensure each crate being released has release notes for its target version.
  ///
  /// A crate passes if either:
  /// - changelog generation is disabled for that crate, or
  /// - changelog already contains `## [<version>]`, or
  /// - generated changelog entries for this release are non-empty.
  fn validate_release_notes(&self, plan: &ReleasePlan) -> RailResult<()> {
    for crate_plan in &plan.crates {
      if !crate_plan.generate_changelog {
        continue;
      }

      if self.release_notes_override_exists(crate_plan) {
        continue;
      }

      if changelog_contains_version_entry(&crate_plan.changelog_path, &crate_plan.new_version.to_string()) {
        continue;
      }

      if crate_plan.changelog_body.trim().is_empty() {
        return Err(RailError::with_help(
          format!(
            "no release notes for {} v{} in {}",
            crate_plan.name,
            crate_plan.new_version,
            crate_plan.changelog_path.display()
          ),
          "add user-facing commits, pre-populate the version section, or set [release].require_release_notes = false",
        ));
      }
    }

    Ok(())
  }

  fn release_notes_override_exists(&self, crate_plan: &crate::release::planner::CrateReleasePlan) -> bool {
    let Some(config) = self.ctx.config.as_ref() else {
      return false;
    };
    let dir = self.ctx.workspace_root().join(&config.release.release_notes_dir);
    dir.join(format!("v{}.md", crate_plan.new_version)).exists()
      || dir.join(format!("{}.md", crate_plan.tag_name)).exists()
  }

  /// Check for path dependencies (which block publishing)
  ///
  /// This check is skipped for non-publishable crates since path-only
  /// dependencies only matter for crates going to crates.io.
  fn check_path_dependencies(&self, crate_name: &str) -> RailResult<()> {
    // Skip this check for non-publishable crates - path deps don't matter
    // if the crate will never be published to crates.io
    if !self.is_publishable(crate_name) {
      return Ok(());
    }

    let package = self
      .ctx
      .cargo
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    for dep in &package.dependencies {
      if dep.path.is_some() {
        // Allow dev-dependencies with paths (tests can use local crates)
        if dep.kind == cargo_metadata::DependencyKind::Development {
          continue;
        }

        // Allow path deps that also have a version requirement
        // These are workspace path deps: { version = "x.y", path = "../foo" }
        // Cargo will use the version when publishing, not the path
        let has_version = !dep.req.comparators.is_empty();
        if has_version {
          continue;
        }

        // Pure path-only dependencies cannot be published
        return Err(RailError::with_help(
          format!("Crate '{}' has path-only dependency '{}'", crate_name, dep.name),
          "Path-only dependencies cannot be published. Add a version: { version = \"x.y\", path = \"...\" }",
        ));
      }
    }

    Ok(())
  }

  /// Check if a crate is publishable (combined Cargo.toml + rail.toml check)
  ///
  /// A crate is considered publishable if:
  /// 1. Cargo.toml does not have `publish = false`, AND
  /// 2. rail.toml does not have `[crates.NAME.release] publish = false`
  ///
  /// rail.toml takes precedence: if it explicitly sets `publish = true`,
  /// that overrides Cargo.toml's `publish = false`.
  pub fn is_publishable(&self, crate_name: &str) -> bool {
    let package = match self.ctx.cargo.get_package(crate_name) {
      Some(pkg) => pkg,
      None => return false,
    };

    // Check Cargo.toml
    let publish_from_cargo = crate::workspace::CargoState::is_package_publishable(package);

    // Check rail.toml - takes precedence if set
    let publish_from_config = self
      .ctx
      .config
      .as_ref()
      .and_then(|c| c.crates.get(crate_name))
      .and_then(|c| c.release.as_ref())
      .map(|r| r.publish);

    // rail.toml takes precedence if explicitly set
    publish_from_config.unwrap_or(publish_from_cargo)
  }

  /// Get the reason why a crate is not publishable
  ///
  /// Returns `None` if the crate is publishable.
  pub fn unpublishable_reason(&self, crate_name: &str) -> Option<String> {
    let package = match self.ctx.cargo.get_package(crate_name) {
      Some(pkg) => pkg,
      None => return Some(format!("crate '{}' not found", crate_name)),
    };

    // Check rail.toml first (takes precedence)
    if let Some(config) = &self.ctx.config
      && let Some(crate_config) = config.crates.get(crate_name)
      && let Some(release_config) = &crate_config.release
    {
      // If rail.toml explicitly sets publish, use that
      if !release_config.publish {
        return Some("publish = false in rail.toml".to_string());
      }
      // If rail.toml explicitly sets publish = true, it's publishable
      // (overrides Cargo.toml)
      return None;
    }

    // Fall back to Cargo.toml check
    if !crate::workspace::CargoState::is_package_publishable(package) {
      return Some("publish = false in Cargo.toml".to_string());
    }

    None
  }

  /// Filter workspace members to only publishable crates
  ///
  /// Produces `(publishable_crates, skipped_with_reason)` for workspace members.
  pub fn publishable_members(&self) -> (Vec<String>, Vec<(String, String)>) {
    let all_members = self.ctx.graph.workspace_members();
    let member_count = all_members.len();
    let mut publishable = Vec::with_capacity(member_count);
    let mut skipped = Vec::with_capacity(member_count / 4); // Most crates are publishable

    for name in all_members {
      if let Some(reason) = self.unpublishable_reason(name) {
        skipped.push((name.clone(), reason));
      } else {
        publishable.push(name.clone());
      }
    }

    (publishable, skipped)
  }

  /// Run `cargo publish --dry-run` to validate package can be published
  ///
  /// This catches issues like:
  /// - Missing required Cargo.toml fields
  /// - Invalid README paths
  /// - Package size limits
  /// - Files that would be excluded
  pub fn validate_publish_dry_run(&self, crate_name: &str) -> ValidationResult {
    let package = match self.ctx.cargo.get_package(crate_name) {
      Some(pkg) => pkg,
      None => return ValidationResult::failed("publish-dry-run", format!("crate '{}' not found", crate_name)),
    };

    let crate_dir = match package.manifest_path.parent() {
      Some(dir) => dir,
      None => return ValidationResult::failed("publish-dry-run", "invalid manifest path"),
    };

    // Run cargo publish --dry-run
    let output = process::run(
      "cargo",
      &["publish", "--dry-run", "--allow-dirty"],
      Some(crate_dir.as_std_path()),
    );

    match output {
      Ok(result) => {
        if result.status.success() {
          ValidationResult::passed("publish-dry-run", "package is valid for publishing")
        } else {
          let stderr = String::from_utf8_lossy(&result.stderr);
          // Extract the meaningful error message (skip the "error:" prefix noise)
          let error_msg = stderr
            .lines()
            .find(|line| line.contains("error") || line.contains("Error"))
            .unwrap_or(&stderr)
            .trim();
          ValidationResult::failed("publish-dry-run", error_msg.to_string())
        }
      }
      Err(e) => ValidationResult::failed("publish-dry-run", format!("failed to run cargo: {}", e)),
    }
  }

  /// Verify the crate can be built with the declared MSRV
  ///
  /// If the workspace manifest has `rust-version`, this runs `cargo check`
  /// with that toolchain to ensure compatibility.
  pub fn validate_msrv(&self, crate_name: &str) -> ValidationResult {
    let package = match self.ctx.cargo.get_package(crate_name) {
      Some(pkg) => pkg,
      None => return ValidationResult::failed("msrv", format!("crate '{}' not found", crate_name)),
    };

    // Get MSRV from package or workspace
    let msrv = package.rust_version.as_ref();

    let msrv_str = match msrv {
      Some(v) => v.to_string(),
      None => {
        // No MSRV declared - check passes (nothing to verify)
        return ValidationResult::passed("msrv", "no rust-version declared (skipped)");
      }
    };

    let crate_dir = match package.manifest_path.parent() {
      Some(dir) => dir,
      None => return ValidationResult::failed("msrv", "invalid manifest path"),
    };

    // Check if the MSRV toolchain is available
    let toolchain = format!("+{}", msrv_str);
    let check_toolchain = process::run("rustup", &["run", &msrv_str, "rustc", "--version"], None);

    match check_toolchain {
      Ok(result) if !result.status.success() => {
        // Toolchain not installed - skip with warning
        return ValidationResult::passed(
          "msrv",
          format!(
            "rust {} not installed (skipped, install with: rustup install {})",
            msrv_str, msrv_str
          ),
        );
      }
      Err(_) => {
        return ValidationResult::passed("msrv", "rustup not available (skipped)");
      }
      _ => {}
    }

    // Run cargo check with the MSRV toolchain
    let output = process::run(
      "cargo",
      &[&toolchain, "check", "--lib", "--quiet"],
      Some(crate_dir.as_std_path()),
    );

    match output {
      Ok(result) => {
        if result.status.success() {
          ValidationResult::passed("msrv", format!("builds successfully with rust {}", msrv_str))
        } else {
          let stderr = String::from_utf8_lossy(&result.stderr);
          // Get first error line
          let error_msg = stderr
            .lines()
            .find(|line| line.contains("error"))
            .unwrap_or("compilation failed")
            .trim();
          ValidationResult::failed("msrv", format!("fails with rust {}: {}", msrv_str, error_msg))
        }
      }
      Err(e) => ValidationResult::failed("msrv", format!("failed to run cargo: {}", e)),
    }
  }

  /// Run cargo-semver-checks as an installed external binary.
  ///
  /// Only a confirmed breaking verdict can fail the check (under "deny").
  /// Inconclusive runs — no published baseline, network or build failures —
  /// report as skipped so a crate's first release never fails on a
  /// comparison that cannot exist.
  fn validate_semver_checks(&self, crate_name: &str, policy: SemverCheckPolicy) -> ValidationResult {
    use crate::release::semver_checks::SemverCheck;

    match semver_checks::check_release(self.ctx, crate_name) {
      Ok(SemverCheck::Pass) => ValidationResult::passed("semver-checks", "no semver-breaking API changes detected"),
      Ok(SemverCheck::Breaking { message }) => {
        if policy == SemverCheckPolicy::Deny {
          ValidationResult::failed("semver-checks", message)
        } else {
          ValidationResult::passed("semver-checks", format!("advisory: {}", message))
        }
      }
      Ok(SemverCheck::Inconclusive { message }) => {
        ValidationResult::passed("semver-checks", format!("skipped ({})", message))
      }
      Err(e) => ValidationResult::passed("semver-checks", format!("skipped (failed to run: {})", e)),
    }
  }

  /// Run extended validation checks (dry-run publish, MSRV, optional semver checks)
  ///
  /// Runs all checks without fail-fast and returns grouped validation results.
  pub fn validate_extended(
    &self,
    crate_names: &[String],
    semver_policy: SemverCheckPolicy,
  ) -> Vec<(String, Vec<ValidationResult>)> {
    let semver_available =
      semver_policy == SemverCheckPolicy::Off || semver_checks::is_available(self.ctx.workspace_root());
    let mut emitted_semver_missing = false;

    crate_names
      .iter()
      .map(|crate_name| {
        let mut results = vec![
          self.validate_publish_dry_run(crate_name),
          self.validate_msrv(crate_name),
        ];

        if semver_policy != SemverCheckPolicy::Off {
          if !semver_available {
            if !emitted_semver_missing {
              results.push(ValidationResult::passed(
                "semver-checks",
                "cargo-semver-checks not installed (skipped; install with: cargo install cargo-semver-checks)",
              ));
              emitted_semver_missing = true;
            }
          } else if self.is_publishable(crate_name) && semver_checks::has_library_target(self.ctx, crate_name) {
            // Unpublished crates have no crates.io baseline to compare against.
            results.push(self.validate_semver_checks(crate_name, semver_policy));
          }
        }

        (crate_name.clone(), results)
      })
      .collect()
  }

  /// Validate changelog paths for all crates being released
  ///
  /// Checks that:
  /// - Changelog paths don't escape the workspace (no ".." traversal above root)
  /// - Resolved paths are within workspace bounds
  pub fn validate_changelog_paths(&self, crate_names: &[String], release_config: &ReleaseConfig) -> RailResult<()> {
    for crate_name in crate_names {
      // Check per-crate skip setting
      if let Some(config) = &self.ctx.config
        && let Some(crate_config) = config.crates.get(crate_name)
        && let Some(changelog_cfg) = &crate_config.changelog
        && changelog_cfg.skip
      {
        continue;
      }

      let changelog_path = self.resolve_changelog_path(crate_name, release_config)?;

      // Check for path traversal outside workspace
      self.validate_path_within_workspace(&changelog_path, crate_name)?;
    }

    Ok(())
  }

  /// Resolve changelog path for a crate (mirrors planner logic)
  fn resolve_changelog_path(&self, crate_name: &str, release_config: &ReleaseConfig) -> RailResult<PathBuf> {
    let package = self
      .ctx
      .cargo
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    let manifest_path = package.manifest_path.as_std_path();

    let changelog_config = self
      .ctx
      .config
      .as_ref()
      .and_then(|c| c.crates.get(crate_name))
      .and_then(|c| c.changelog.as_ref());

    let changelog_relative_path = changelog_config
      .and_then(|ch| ch.path.as_ref())
      .map(|p| p.to_string_lossy().to_string())
      .unwrap_or_else(|| release_config.changelog.path.clone());
    let relative_to = changelog_config
      .and_then(|ch| ch.relative_to)
      .unwrap_or(release_config.changelog.relative_to);

    // Resolve based on release.changelog.relative_to or per-crate override.
    let changelog_path = match relative_to {
      ChangelogRelativeTo::Crate => manifest_path
        .parent()
        .ok_or_else(|| RailError::message("Invalid manifest path"))?
        .join(&changelog_relative_path),
      ChangelogRelativeTo::Workspace => self.ctx.workspace_root().join(&changelog_relative_path),
    };

    Ok(changelog_path)
  }

  /// Validate that a path is within the workspace bounds
  fn validate_path_within_workspace(&self, path: &std::path::Path, crate_name: &str) -> RailResult<()> {
    let workspace_root = self.ctx.workspace_root();

    // Check for ".." in the path string (simple check)
    let path_str = path.to_string_lossy();
    if path_str.contains("..") {
      // More thorough check: canonicalize if possible to see if it escapes
      // If the path doesn't exist yet, we check by normalizing components
      let normalized = normalize_path(path);
      let workspace_canonical = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

      // Check if normalized path starts with workspace root
      if !normalized.starts_with(&workspace_canonical) && !normalized.starts_with(workspace_root) {
        return Err(RailError::with_help(
          format!(
            "Changelog path for '{}' escapes workspace: {}",
            crate_name,
            path.display()
          ),
          "Ensure changelog paths stay within the workspace directory",
        ));
      }
    }

    Ok(())
  }
}

/// Normalize a path by resolving `.` and `..` components without requiring the path to exist
fn normalize_path(path: &std::path::Path) -> PathBuf {
  use std::path::Component;

  let mut components = Vec::new();

  for component in path.components() {
    match component {
      Component::Prefix(p) => components.push(Component::Prefix(p)),
      Component::RootDir => {
        components.clear();
        components.push(Component::RootDir);
      }
      Component::CurDir => {}
      Component::ParentDir => {
        if let Some(Component::Normal(_)) = components.last() {
          components.pop();
        } else if components.is_empty() || matches!(components.last(), Some(Component::ParentDir)) {
          components.push(Component::ParentDir);
        }
      }
      Component::Normal(c) => components.push(Component::Normal(c)),
    }
  }

  components.iter().collect()
}

fn changelog_contains_version_entry(path: &std::path::Path, version: &str) -> bool {
  let Ok(contents) = fs::read_to_string(path) else {
    return false;
  };
  let needle = format!("## [{}]", version);
  contents.lines().any(|line| line.trim_start().starts_with(&needle))
}
