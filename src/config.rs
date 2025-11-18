use crate::error::{ConfigError, RailError, RailResult, ResultExt};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailConfig {
  pub workspace: WorkspaceConfig,
  #[serde(default)]
  pub security: SecurityConfig,
  #[serde(default)]
  pub policy: PolicyConfig,
  #[serde(default)]
  pub toolchain: ToolchainConfig,
  #[serde(default)]
  pub unify: UnifyConfig,
  #[serde(default)]
  pub splits: Vec<SplitConfig>,
  #[serde(default)]
  pub releases: Vec<ReleaseConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
  pub root: PathBuf,
}

/// Security configuration for mono↔remote syncing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
  /// SSH key path (default: ~/.ssh/id_ed25519 or ~/.ssh/id_rsa)
  #[serde(default)]
  pub ssh_key_path: Option<PathBuf>,

  /// Require SSH signing key for commits (optional, default: false)
  #[serde(default)]
  pub require_signed_commits: bool,

  /// SSH signing key path (default: same as ssh_key_path)
  #[serde(default)]
  pub signing_key_path: Option<PathBuf>,

  /// PR branch pattern for remote→mono syncs (default: "rail/sync/{crate}/{timestamp}")
  #[serde(default = "default_pr_branch_pattern")]
  pub pr_branch_pattern: String,

  /// Protected branches that cannot be directly committed to (default: ["main", "master"])
  #[serde(default = "default_protected_branches")]
  pub protected_branches: Vec<String>,
}

fn default_pr_branch_pattern() -> String {
  "rail/sync/{crate}/{timestamp}".to_string()
}

fn default_protected_branches() -> Vec<String> {
  vec!["main".to_string(), "master".to_string()]
}

impl Default for SecurityConfig {
  fn default() -> Self {
    Self {
      ssh_key_path: None,
      require_signed_commits: false,
      signing_key_path: None,
      pr_branch_pattern: default_pr_branch_pattern(),
      protected_branches: default_protected_branches(),
    }
  }
}

/// Workspace policy configuration (Pillar 3: Policy & Linting)
/// Defines rules and constraints for the workspace
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyConfig {
  /// Cargo resolver version to enforce (e.g., "2" or "3")
  #[serde(default)]
  pub resolver: Option<String>,

  /// Minimum Rust version (MSRV) to enforce
  #[serde(default)]
  pub msrv: Option<String>,

  /// Rust edition to enforce across all crates
  #[serde(default)]
  pub edition: Option<String>,

  /// Dependencies that must not have multiple versions
  /// e.g., ["tokio", "serde", "anyhow"]
  #[serde(default)]
  pub forbid_multiple_versions: Vec<String>,

  /// Require workspace dependency inheritance
  /// If true, all dependencies should use workspace.dependencies
  #[serde(default)]
  pub require_workspace_inheritance: bool,

  /// Allowed licenses (SPDX identifiers)
  /// Empty = no restriction
  #[serde(default)]
  pub allowed_licenses: Vec<String>,

  /// Forbidden `[patch]` or `[replace]` usage (strict mode)
  #[serde(default)]
  pub forbid_patch_replace: bool,
}

impl PolicyConfig {
  /// Validate policy configuration
  pub fn validate(&self) -> RailResult<()> {
    // Validate resolver version if specified
    if let Some(ref resolver) = self.resolver {
      match resolver.as_str() {
        "1" | "2" | "3" => {}
        _ => {
          return Err(RailError::message(format!(
            "Invalid resolver version '{}'. Must be '1', '2', or '3'",
            resolver
          )));
        }
      }
    }

    // Validate MSRV format if specified
    if let Some(ref msrv) = self.msrv
      && semver::Version::parse(msrv).is_err()
    {
      return Err(RailError::message(format!(
        "Invalid MSRV '{}'. Must be valid semver (e.g., '1.76.0')",
        msrv
      )));
    }

    // Validate edition if specified
    if let Some(ref edition) = self.edition {
      match edition.as_str() {
        "2015" | "2018" | "2021" | "2024" => {}
        _ => {
          return Err(RailError::message(format!(
            "Invalid edition '{}'. Must be '2015', '2018', '2021', or '2024'",
            edition
          )));
        }
      }
    }

    Ok(())
  }

  /// Check if policy is enabled (any field is set) - public API for conditional logic
  #[allow(dead_code)]
  pub fn is_enabled(&self) -> bool {
    self.resolver.is_some()
      || self.msrv.is_some()
      || self.edition.is_some()
      || !self.forbid_multiple_versions.is_empty()
      || self.require_workspace_inheritance
      || !self.allowed_licenses.is_empty()
      || self.forbid_patch_replace
  }
}

/// Toolchain configuration - source of truth for rust-toolchain.toml
///
/// This configuration drives `cargo rail config sync` to generate/validate rust-toolchain.toml
/// Supports all rust-toolchain.toml fields as documented in rustup docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainConfig {
  /// Rust channel (stable, beta, nightly, or specific version like "1.76.0")
  /// Mutually exclusive with `path`. If both are set, validation will fail.
  #[serde(default = "default_channel")]
  pub channel: String,

  /// Path to custom local toolchain (absolute path)
  /// Mutually exclusive with `channel`. When set, components and targets have no effect.
  #[serde(default)]
  pub path: Option<String>,

  /// Toolchain profile (minimal, default, complete)
  #[serde(default = "default_profile")]
  pub profile: String,

  /// Additional components (e.g., clippy, rustfmt, rust-src)
  /// Additive to the current profile. No effect if `path` is set.
  #[serde(default)]
  pub components: Vec<String>,

  /// Target triples for cross-compilation
  /// Used by unify for optional validation and by config sync for rust-toolchain.toml
  /// The host platform is automatically included. No effect if `path` is set.
  #[serde(default)]
  pub targets: Vec<String>,
}

fn default_channel() -> String {
  "stable".to_string()
}

fn default_profile() -> String {
  "default".to_string()
}

impl Default for ToolchainConfig {
  fn default() -> Self {
    Self {
      channel: default_channel(),
      path: None,
      profile: default_profile(),
      components: vec![],
      targets: vec![],
    }
  }
}

impl ToolchainConfig {
  /// Validate toolchain configuration
  pub fn validate(&self) -> RailResult<()> {
    // Validate mutual exclusivity: channel and path cannot both be set
    if self.path.is_some() && !self.channel.is_empty() && self.channel != "stable" {
      // Allow default "stable" to coexist with path (path takes precedence)
      // But if user explicitly sets a non-default channel, that's an error
      return Err(RailError::message(
        "Toolchain 'channel' and 'path' are mutually exclusive. Use one or the other.",
      ));
    }

    // Validate profile
    match self.profile.as_str() {
      "minimal" | "default" | "complete" => {}
      _ => {
        return Err(RailError::message(format!(
          "Invalid toolchain profile '{}'. Must be 'minimal', 'default', or 'complete'",
          self.profile
        )));
      }
    }

    // Validate channel format (basic check) - only if not using path
    if self.path.is_none() && self.channel.is_empty() {
      return Err(RailError::message("Toolchain channel cannot be empty"));
    }

    // Validate path if specified
    if let Some(ref path) = self.path
      && path.is_empty()
    {
      return Err(RailError::message("Toolchain path cannot be empty"));
    }
    // Note: We don't validate that the path exists here, as it might not exist yet
    // or might be created after config is written. rustup will validate it.

    Ok(())
  }
}

/// Unify configuration - controls workspace dependency unification behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifyConfig {
  /// Use --all-features when gathering metadata (default: true)
  /// This ensures the feature union across all workspace members is captured
  #[serde(default = "default_use_all_features")]
  pub use_all_features: bool,

  /// Automatically sync rust-toolchain.toml before unify runs (default: true)
  #[serde(default = "default_sync_on_unify")]
  pub sync_on_unify: bool,

  /// Optional: validate unification against specific target triples
  /// When enabled, runs parallel metadata checks per target to catch platform-specific issues
  /// Examples: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
  #[serde(default)]
  pub validate_targets: Vec<String>,

  /// Maximum parallel jobs for validation (0 = auto-detect CPUs, >0 = explicit limit)
  #[serde(default)]
  pub max_parallel_jobs: usize,
}

fn default_use_all_features() -> bool {
  true
}

fn default_sync_on_unify() -> bool {
  true
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      use_all_features: default_use_all_features(),
      sync_on_unify: default_sync_on_unify(),
      validate_targets: vec![],
      max_parallel_jobs: 0, // Auto-detect
    }
  }
}

impl UnifyConfig {
  /// Check if target validation is enabled
  pub fn validation_enabled(&self) -> bool {
    !self.validate_targets.is_empty()
  }

  /// Get effective parallel job count (auto-detect if 0)
  pub fn effective_parallelism(&self) -> usize {
    if self.max_parallel_jobs == 0 {
      // Auto-detect: use number of logical CPUs
      std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
      self.max_parallel_jobs
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitConfig {
  pub name: String,
  pub remote: String,
  pub branch: String,
  pub mode: SplitMode,
  /// For combined mode: how to structure the split repo
  #[serde(default)]
  pub workspace_mode: WorkspaceMode,
  #[serde(default)]
  pub paths: Vec<CratePath>,
  #[serde(default)]
  pub include: Vec<String>,
  #[serde(default)]
  pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CratePath {
  #[serde(rename = "crate")]
  pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SplitMode {
  #[default]
  Single,
  Combined,
}

/// How to structure a combined split repository
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceMode {
  /// Multiple standalone crates in one repo (no workspace structure)
  #[default]
  Standalone,
  /// Workspace structure with root Cargo.toml (mirrors monorepo)
  Workspace,
}

/// Visibility tier for releases (OSS/internal/enterprise)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Visibility {
  /// Open source release (publicly accessible)
  Oss,
  /// Internal release (company-internal use)
  #[default]
  Internal,
  /// Enterprise release (commercial offering)
  Enterprise,
}

/// Release configuration for a crate or product
///
/// # Invariants
///
/// 1. Releases always driven from monorepo (not from splits)
/// 2. Each release has: name, version, last_sha anchor
/// 3. Changelogs are per-thing (per crate/product)
///
/// # Example
///
/// ```toml
/// [[releases]]
/// name = "lib-core"
/// crate = "crates/lib-core"
/// split = "lib_core"  # optional: link to splits config
/// changelog = "crates/lib-core/CHANGELOG.md"
/// visibility = "oss"
/// includes = ["lib-core", "lib-utils"]  # for bundled products
/// last_version = "0.3.1"
/// last_sha = "abc123..."
/// last_date = "2025-01-15"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseConfig {
  /// Unique name for this release channel
  pub name: String,

  /// Path to the crate directory (relative to workspace root)
  #[serde(rename = "crate")]
  pub crate_path: PathBuf,

  /// Optional link to splits configuration
  /// If set, releases will be synced to the split repo
  #[serde(default)]
  pub split: Option<String>,

  /// Path to changelog file (e.g., "crates/lib-core/CHANGELOG.md")
  /// Used by `cargo rail release apply` to generate changelog entries
  #[serde(default)]
  pub changelog: Option<PathBuf>,

  /// Release visibility tier (oss, internal, enterprise)
  /// Enables tier-based graph filtering and tier violation checks
  #[serde(default)]
  pub visibility: Visibility,

  /// List of crate names included in this release
  /// For products that bundle multiple crates together
  #[serde(default)]
  pub includes: Vec<String>,

  /// Last released version (updated by `cargo rail release apply`)
  #[serde(default)]
  pub last_version: Option<String>,

  /// Git SHA of last release (anchor point for next release)
  #[serde(default)]
  pub last_sha: Option<String>,

  /// Date of last release (ISO 8601 format)
  #[serde(default)]
  pub last_date: Option<String>,
}

impl ReleaseConfig {
  /// Validate release configuration
  pub fn validate(&self, workspace_root: &Path) -> RailResult<()> {
    // Validate crate path exists
    let crate_abs_path = workspace_root.join(&self.crate_path);
    if !crate_abs_path.exists() {
      return Err(RailError::with_help(
        format!(
          "Release '{}': crate path '{}' does not exist",
          self.name,
          self.crate_path.display()
        ),
        "Ensure the crate path is correct in rail.toml",
      ));
    }

    // Validate changelog path if specified
    if let Some(ref changelog) = self.changelog {
      // Check for valid extension
      if let Some(ext) = changelog.extension() {
        if ext != "md" {
          return Err(RailError::with_help(
            format!(
              "Release '{}': changelog must be a Markdown file (*.md), found '{}'",
              self.name,
              changelog.display()
            ),
            "Use a .md extension for changelog files",
          ));
        }
      } else {
        return Err(RailError::with_help(
          format!(
            "Release '{}': changelog path '{}' has no file extension",
            self.name,
            changelog.display()
          ),
          "Changelog must be a Markdown file (*.md)",
        ));
      }

      // Validate parent directory exists (file itself can be created later)
      let changelog_abs = workspace_root.join(changelog);
      if let Some(parent) = changelog_abs.parent()
        && !parent.exists()
      {
        return Err(RailError::with_help(
          format!(
            "Release '{}': changelog parent directory '{}' does not exist",
            self.name,
            parent.display()
          ),
          "Create the directory or adjust the changelog path",
        ));
      }
    }

    // Validate includes are not empty strings
    for include in &self.includes {
      if include.trim().is_empty() {
        return Err(RailError::with_help(
          format!("Release '{}': includes cannot contain empty strings", self.name),
          "Remove empty strings from the includes list",
        ));
      }
    }

    Ok(())
  }
}

impl RailConfig {
  /// Find config file in search order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
  pub fn find_config_path(path: &Path) -> Option<PathBuf> {
    let candidates = vec![
      path.join("rail.toml"),
      path.join(".rail.toml"),
      path.join(".cargo").join("rail.toml"),
      path.join(".config").join("rail.toml"),
    ];

    candidates.into_iter().find(|p| p.exists())
  }

  /// Load config from rail.toml (searches multiple locations)
  pub fn load(path: &Path) -> RailResult<Self> {
    let config_path = Self::find_config_path(path).ok_or_else(|| {
      RailError::Config(ConfigError::NotFound {
        workspace_root: path.to_path_buf(),
      })
    })?;

    let content = fs::read_to_string(&config_path)
      .with_context(|| format!("Failed to read config from {}", config_path.display()))?;
    let config: RailConfig = toml_edit::de::from_str(&content)
      .with_context(|| format!("Failed to parse config from {}", config_path.display()))?;

    // Validate policy configuration
    config
      .policy
      .validate()
      .with_context(|| format!("Invalid policy configuration in {}", config_path.display()))?;

    // Validate toolchain configuration
    config
      .toolchain
      .validate()
      .with_context(|| format!("Invalid toolchain configuration in {}", config_path.display()))?;

    // Validate release configurations
    let workspace_root = &config.workspace.root;
    for release in &config.releases {
      release
        .validate(workspace_root)
        .with_context(|| format!("Invalid release configuration in {}", config_path.display()))?;
    }

    Ok(config)
  }
}

impl SplitConfig {
  /// Get the path(s) for this split configuration
  pub fn get_paths(&self) -> Vec<&PathBuf> {
    self.paths.iter().map(|cp| &cp.path).collect()
  }

  /// Validate the split configuration
  pub fn validate(&self) -> RailResult<()> {
    // Check paths exist
    if self.paths.is_empty() {
      return Err(RailError::with_help(
        format!("Split '{}' must have at least one crate path", self.name),
        "Add at least one crate path in rail.toml under [[splits]]",
      ));
    }

    // Check remote is not empty
    if self.remote.is_empty() {
      return Err(RailError::Config(ConfigError::MissingField {
        field: format!("remote for split '{}'", self.name),
      }));
    }

    // Validate mode-specific requirements
    match self.mode {
      SplitMode::Single => {
        if self.paths.len() != 1 {
          return Err(RailError::with_help(
            format!(
              "Single mode split '{}' must have exactly one path (found {})",
              self.name,
              self.paths.len()
            ),
            "Change mode to 'combined' or remove extra paths",
          ));
        }
      }
      SplitMode::Combined => {
        if self.paths.len() < 2 {
          return Err(RailError::with_help(
            format!(
              "Combined mode split '{}' should have multiple paths (found {})",
              self.name,
              self.paths.len()
            ),
            "Change mode to 'single' or add more crate paths",
          ));
        }
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_policy_config_validation_valid() {
    let policy = PolicyConfig {
      resolver: Some("2".to_string()),
      msrv: Some("1.76.0".to_string()),
      edition: Some("2024".to_string()),
      ..Default::default()
    };
    assert!(policy.validate().is_ok());
  }

  #[test]
  fn test_policy_config_validation_invalid_resolver() {
    let policy = PolicyConfig {
      resolver: Some("5".to_string()),
      ..Default::default()
    };
    assert!(policy.validate().is_err());
  }

  #[test]
  fn test_policy_config_validation_invalid_msrv() {
    let policy = PolicyConfig {
      msrv: Some("invalid".to_string()),
      ..Default::default()
    };
    assert!(policy.validate().is_err());
  }

  #[test]
  fn test_policy_config_validation_invalid_edition() {
    let policy = PolicyConfig {
      edition: Some("2099".to_string()),
      ..Default::default()
    };
    assert!(policy.validate().is_err());
  }

  #[test]
  fn test_policy_config_is_enabled() {
    let policy_disabled = PolicyConfig::default();
    assert!(!policy_disabled.is_enabled());

    let policy_enabled = PolicyConfig {
      resolver: Some("2".to_string()),
      ..Default::default()
    };
    assert!(policy_enabled.is_enabled());
  }

  #[test]
  fn test_visibility_default() {
    assert_eq!(Visibility::default(), Visibility::Internal);
  }

  #[test]
  fn test_visibility_serialization() {
    assert_eq!(serde_json::to_string(&Visibility::Oss).unwrap(), "\"oss\"");
    assert_eq!(serde_json::to_string(&Visibility::Internal).unwrap(), "\"internal\"");
    assert_eq!(
      serde_json::to_string(&Visibility::Enterprise).unwrap(),
      "\"enterprise\""
    );
  }

  #[test]
  fn test_release_config_validation_valid() {
    use std::env;
    use std::fs;

    let temp_dir = env::temp_dir().join("cargo-rail-test-release-valid");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let crate_dir = temp_dir.join("my-crate");
    fs::create_dir_all(&crate_dir).unwrap();

    let release = ReleaseConfig {
      name: "test-release".to_string(),
      crate_path: PathBuf::from("my-crate"),
      split: None,
      changelog: Some(PathBuf::from("my-crate/CHANGELOG.md")),
      visibility: Visibility::Oss,
      includes: vec!["my-crate".to_string(), "other-crate".to_string()],
      last_version: None,
      last_sha: None,
      last_date: None,
    };

    assert!(release.validate(&temp_dir).is_ok());
    let _ = fs::remove_dir_all(&temp_dir);
  }

  #[test]
  fn test_release_config_validation_invalid_crate_path() {
    use std::env;
    use std::fs;

    let temp_dir = env::temp_dir().join("cargo-rail-test-release-invalid-path");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let release = ReleaseConfig {
      name: "test-release".to_string(),
      crate_path: PathBuf::from("nonexistent-crate"),
      split: None,
      changelog: None,
      visibility: Visibility::Internal,
      includes: vec![],
      last_version: None,
      last_sha: None,
      last_date: None,
    };

    assert!(release.validate(&temp_dir).is_err());
    let _ = fs::remove_dir_all(&temp_dir);
  }

  #[test]
  fn test_release_config_validation_invalid_changelog_extension() {
    use std::env;
    use std::fs;

    let temp_dir = env::temp_dir().join("cargo-rail-test-release-invalid-changelog");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let crate_dir = temp_dir.join("my-crate");
    fs::create_dir_all(&crate_dir).unwrap();

    let release = ReleaseConfig {
      name: "test-release".to_string(),
      crate_path: PathBuf::from("my-crate"),
      split: None,
      changelog: Some(PathBuf::from("my-crate/CHANGELOG.txt")),
      visibility: Visibility::Oss,
      includes: vec![],
      last_version: None,
      last_sha: None,
      last_date: None,
    };

    assert!(release.validate(&temp_dir).is_err());
    let _ = fs::remove_dir_all(&temp_dir);
  }

  #[test]
  fn test_release_config_validation_invalid_changelog_parent() {
    use std::env;
    use std::fs;

    let temp_dir = env::temp_dir().join("cargo-rail-test-release-invalid-parent");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let crate_dir = temp_dir.join("my-crate");
    fs::create_dir_all(&crate_dir).unwrap();

    let release = ReleaseConfig {
      name: "test-release".to_string(),
      crate_path: PathBuf::from("my-crate"),
      split: None,
      changelog: Some(PathBuf::from("nonexistent/CHANGELOG.md")),
      visibility: Visibility::Oss,
      includes: vec![],
      last_version: None,
      last_sha: None,
      last_date: None,
    };

    assert!(release.validate(&temp_dir).is_err());
    let _ = fs::remove_dir_all(&temp_dir);
  }

  #[test]
  fn test_release_config_validation_empty_includes() {
    use std::env;
    use std::fs;

    let temp_dir = env::temp_dir().join("cargo-rail-test-release-empty-includes");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let crate_dir = temp_dir.join("my-crate");
    fs::create_dir_all(&crate_dir).unwrap();

    let release = ReleaseConfig {
      name: "test-release".to_string(),
      crate_path: PathBuf::from("my-crate"),
      split: None,
      changelog: None,
      visibility: Visibility::Internal,
      includes: vec!["valid".to_string(), "  ".to_string()],
      last_version: None,
      last_sha: None,
      last_date: None,
    };

    assert!(release.validate(&temp_dir).is_err());
    let _ = fs::remove_dir_all(&temp_dir);
  }

  // ============================================================================
  // Toolchain Config Tests
  // ============================================================================

  #[test]
  fn test_toolchain_config_default() {
    let toolchain = ToolchainConfig::default();
    assert_eq!(toolchain.channel, "stable");
    assert_eq!(toolchain.profile, "default");
    assert!(toolchain.components.is_empty());
    assert!(toolchain.targets.is_empty());
  }

  #[test]
  fn test_toolchain_config_validation_valid() {
    let toolchain = ToolchainConfig {
      channel: "1.76.0".to_string(),
      path: None,
      profile: "minimal".to_string(),
      components: vec!["clippy".to_string(), "rustfmt".to_string()],
      targets: vec!["x86_64-unknown-linux-gnu".to_string()],
    };
    assert!(toolchain.validate().is_ok());
  }

  #[test]
  fn test_toolchain_config_path_mode() {
    let toolchain = ToolchainConfig {
      channel: "stable".to_string(), // Default, allowed with path
      path: Some("/path/to/custom/toolchain".to_string()),
      profile: "default".to_string(),
      components: vec![],
      targets: vec![],
    };
    assert!(toolchain.validate().is_ok());
  }

  #[test]
  fn test_toolchain_config_path_channel_conflict() {
    let toolchain = ToolchainConfig {
      channel: "nightly".to_string(), // Non-default channel conflicts with path
      path: Some("/path/to/custom/toolchain".to_string()),
      profile: "default".to_string(),
      components: vec![],
      targets: vec![],
    };
    assert!(toolchain.validate().is_err());
  }

  #[test]
  fn test_toolchain_config_validation_invalid_profile() {
    let toolchain = ToolchainConfig {
      channel: "stable".to_string(),
      path: None,
      profile: "invalid".to_string(),
      components: vec![],
      targets: vec![],
    };
    assert!(toolchain.validate().is_err());
  }

  #[test]
  fn test_toolchain_config_validation_empty_channel() {
    let toolchain = ToolchainConfig {
      channel: "".to_string(),
      path: None,
      profile: "default".to_string(),
      components: vec![],
      targets: vec![],
    };
    assert!(toolchain.validate().is_err());
  }

  // ============================================================================
  // Unify Config Tests
  // ============================================================================

  #[test]
  fn test_unify_config_default() {
    let unify = UnifyConfig::default();
    assert!(unify.use_all_features);
    assert!(unify.sync_on_unify);
    assert!(unify.validate_targets.is_empty());
    assert_eq!(unify.max_parallel_jobs, 0); // Auto-detect
  }

  #[test]
  fn test_unify_config_validation_enabled() {
    let unify_disabled = UnifyConfig::default();
    assert!(!unify_disabled.validation_enabled());

    let unify_enabled = UnifyConfig {
      validate_targets: vec!["x86_64-unknown-linux-gnu".to_string()],
      ..Default::default()
    };
    assert!(unify_enabled.validation_enabled());
  }

  #[test]
  fn test_unify_config_effective_parallelism_auto() {
    let unify = UnifyConfig {
      max_parallel_jobs: 0, // Auto-detect
      ..Default::default()
    };
    let parallelism = unify.effective_parallelism();
    assert!(parallelism >= 1, "Should detect at least 1 CPU");
  }

  #[test]
  fn test_unify_config_effective_parallelism_explicit() {
    let unify = UnifyConfig {
      max_parallel_jobs: 4,
      ..Default::default()
    };
    assert_eq!(unify.effective_parallelism(), 4);
  }

  #[test]
  fn test_unify_config_serialization() {
    let unify = UnifyConfig {
      use_all_features: true,
      sync_on_unify: false,
      validate_targets: vec!["x86_64-unknown-linux-gnu".to_string()],
      max_parallel_jobs: 2,
    };

    // Serialize to TOML
    let toml = toml_edit::ser::to_string(&unify).unwrap();
    assert!(toml.contains("use_all_features = true"));
    assert!(toml.contains("sync_on_unify = false"));
    assert!(toml.contains("x86_64-unknown-linux-gnu"));
    assert!(toml.contains("max_parallel_jobs = 2"));

    // Deserialize back
    let parsed: UnifyConfig = toml_edit::de::from_str(&toml).unwrap();
    assert_eq!(parsed.use_all_features, unify.use_all_features);
    assert_eq!(parsed.sync_on_unify, unify.sync_on_unify);
    assert_eq!(parsed.validate_targets, unify.validate_targets);
    assert_eq!(parsed.max_parallel_jobs, unify.max_parallel_jobs);
  }
}
