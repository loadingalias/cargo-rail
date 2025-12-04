//! Pre-release validation checks

use crate::config::{ChangelogRelativeTo, ReleaseConfig};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::path::PathBuf;
use std::process::Command;

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

    // 3. Check for uncommitted changes in crate directories
    if require_clean {
      for crate_name in crate_names {
        self.check_crate_uncommitted_changes(crate_name)?;
      }
    }

    // 4. Check for path dependencies and config restrictions
    for crate_name in crate_names {
      self.check_path_dependencies(crate_name)?;
      self.check_config_restrictions(crate_name)?;
    }

    Ok(())
  }

  /// Check if working directory is clean (no uncommitted changes)
  fn check_clean_working_directory(&self) -> RailResult<()> {
    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["status", "--porcelain"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git status: {}", e)))?;

    if !output.status.success() {
      return Err(RailError::message("git status failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
      return Err(RailError::with_help(
        "Working directory has uncommitted changes",
        "Commit or stash your changes before releasing, or set require_clean = false in [release] section of rail.toml",
      ));
    }

    Ok(())
  }

  /// Check for uncommitted changes in specific crate directory
  fn check_crate_uncommitted_changes(&self, crate_name: &str) -> RailResult<()> {
    let package = self
      .ctx
      .cargo
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    let crate_dir = package
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    // Check for changes in this directory
    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["status", "--porcelain", "--"])
      .arg(crate_dir)
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git status: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
      return Err(RailError::with_help(
        format!("Crate '{}' has uncommitted changes", crate_name),
        "Commit changes before releasing",
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

    // Check if publish = false
    if let Some(publish) = &package.publish
      && publish.is_empty()
    {
      return Err(RailError::with_help(
        format!("Crate '{}' has publish = false in Cargo.toml", crate_name),
        "Remove 'publish = false' or exclude this crate from the release",
      ));
    }

    Ok(())
  }

  /// Check for path dependencies (which block publishing)
  fn check_path_dependencies(&self, crate_name: &str) -> RailResult<()> {
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

  /// Check rail.toml config restrictions
  fn check_config_restrictions(&self, crate_name: &str) -> RailResult<()> {
    if let Some(config) = &self.ctx.config
      && let Some(crate_config) = config.crates.get(crate_name)
      && let Some(release_config) = &crate_config.release
      && !release_config.publish
    {
      return Err(RailError::with_help(
        format!("Crate '{}' is configured as non-publishable in rail.toml", crate_name),
        "Update rail.toml [crates.NAME.release] section to allow publishing",
      ));
    }
    Ok(())
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
    let output = Command::new("cargo")
      .current_dir(crate_dir)
      .args(["publish", "--dry-run", "--allow-dirty"])
      .output();

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
    let check_toolchain = Command::new("rustup")
      .args(["run", &msrv_str, "rustc", "--version"])
      .output();

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
    let output = Command::new("cargo")
      .current_dir(crate_dir)
      .args([&toolchain, "check", "--lib", "--quiet"])
      .output();

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

  /// Run extended validation checks (dry-run publish, MSRV)
  ///
  /// Returns a list of validation results. Does not fail fast - runs all checks.
  pub fn validate_extended(&self, crate_names: &[String]) -> Vec<(String, Vec<ValidationResult>)> {
    crate_names
      .iter()
      .map(|crate_name| {
        let results = vec![
          self.validate_publish_dry_run(crate_name),
          self.validate_msrv(crate_name),
        ];
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
      // Skip if changelog is disabled for this crate
      if release_config.skip_changelog_for.iter().any(|c| c == crate_name) {
        continue;
      }

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

    // Get changelog path from per-crate config or global config
    let changelog_relative_path = self
      .ctx
      .config
      .as_ref()
      .and_then(|c| c.crates.get(crate_name))
      .and_then(|c| c.changelog.as_ref())
      .and_then(|ch| ch.path.as_ref())
      .map(|p| p.to_string_lossy().to_string())
      .unwrap_or_else(|| release_config.changelog_path.clone());

    // Resolve based on changelog_relative_to setting
    let changelog_path = match release_config.changelog_relative_to {
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
