//! Release configuration - controls release management behavior

use crate::config::unify::default_true;
use crate::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

/// Release configuration (workspace-wide defaults)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseConfig {
    /// Git tag prefix (default: "v")
    #[serde(default = "default_tag_prefix")]
    pub tag_prefix: String,

    /// Tag format template (default: "{crate}-v{version}" for monorepos, "v{version}" for single crates)
    /// Variables: {crate}, {version}
    #[serde(default = "default_tag_format")]
    pub tag_format: String,

    /// Authorized remote effects after local release preparation.
    #[serde(default)]
    pub remote_effects: ReleaseRemoteEffects,

    /// Registry publication authority, separate from Git and forge effects.
    #[serde(default)]
    pub registry_publication: ReleaseRegistryPublication,

    /// Sign git tags with GPG/SSH (default: false)
    #[serde(default)]
    pub sign_tags: bool,

    /// Directory containing pending change files (default: ".changes").
    ///
    /// Relative to the workspace root.
    #[serde(default = "default_change_dir")]
    pub change_dir: String,

    /// How `--bump auto` maps breaking changes for pre-1.0 crates (default: "minor").
    ///
    /// - "minor": breaking change on 0.x bumps minor (0.3.1 -> 0.4.0)
    /// - "major": breaking change on 0.x bumps major (0.3.1 -> 1.0.0)
    #[serde(default)]
    pub pre_1_breaking_bump: Pre1BreakingBump,

    /// cargo-semver-checks integration policy (default: "warn").
    ///
    /// Requires the `cargo-semver-checks` binary; it is invoked as an external
    /// tool, never vendored. Missing binary downgrades to an advisory note.
    ///
    /// - "off": never run
    /// - "warn": validate planned intent; extended checks remain advisory
    /// - "deny": validate planned intent and fail extended checks on breakage
    #[serde(default)]
    pub semver_check: SemverCheckPolicy,

    /// Lockstep version groups. Each named group lists workspace members that
    /// must be released together at the maximum bump level any member earns.
    #[serde(default)]
    pub version_groups: BTreeMap<String, Vec<String>>,

    /// Standalone Cargo manifests whose committed lockfiles project release versions.
    ///
    /// Each path is workspace-relative and must name `Cargo.toml`. Cargo-Rail
    /// resolves the owning workspace lockfile and plans its exact post-release
    /// bytes without mutating the live checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auxiliary_cargo_manifests: Vec<PathBuf>,

    /// Changelog location and shape (workspace defaults).
    ///
    /// Every key can be overridden per crate under `[crates.NAME.changelog]`.
    #[serde(default)]
    pub changelog: ChangelogShape,
}

impl Default for ReleaseConfig {
    fn default() -> Self {
        Self {
            tag_prefix: default_tag_prefix(),
            tag_format: default_tag_format(),
            remote_effects: ReleaseRemoteEffects::default(),
            registry_publication: ReleaseRegistryPublication::default(),
            sign_tags: false,
            change_dir: default_change_dir(),
            pre_1_breaking_bump: Pre1BreakingBump::default(),
            semver_check: SemverCheckPolicy::default(),
            version_groups: BTreeMap::new(),
            auxiliary_cargo_manifests: Vec::new(),
            changelog: ChangelogShape::default(),
        }
    }
}

impl ReleaseConfig {
    /// Validate the release configuration
    pub fn validate(&self, workspace_members: &[String]) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();

        // Validate tag_format
        if self.tag_format.trim().is_empty() {
            return Err(ConfigError::InvalidField {
                field: "release.tag_format".to_string(),
                reason: "tag_format cannot be empty".to_string(),
            });
        }

        // Check for recommended placeholders in monorepo context
        let is_monorepo = workspace_members.len() > 1;
        if is_monorepo && !self.tag_format.contains("{crate}") {
            warnings.push(
                "release.tag_format does not contain {crate} placeholder. \
                In monorepos, this may cause tag collisions between crates."
                    .to_string(),
            );
        }

        if !self.tag_format.contains("{version}") && !self.tag_format.contains("{prefix}") {
            warnings.push(
                "release.tag_format does not contain {version} or {prefix} placeholder. \
                Tags may not be identifiable."
                    .to_string(),
            );
        }

        validate_change_dir(&self.change_dir)?;

        self.validate_version_groups(workspace_members)?;
        self.validate_auxiliary_cargo_manifests()?;

        Ok(warnings)
    }

    fn validate_auxiliary_cargo_manifests(&self) -> Result<(), ConfigError> {
        let mut previous = None;
        let mut manifests = self.auxiliary_cargo_manifests.iter().collect::<Vec<_>>();
        manifests.sort_unstable();
        for manifest in manifests {
            let valid_components = manifest
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
            if manifest.as_os_str().is_empty()
                || manifest.is_absolute()
                || !valid_components
                || manifest.file_name().is_none_or(|name| name != "Cargo.toml")
            {
                return Err(ConfigError::InvalidField {
                    field: "release.auxiliary_cargo_manifests".to_string(),
                    reason: format!(
                        "'{}' must be a workspace-relative Cargo.toml path without parent traversal",
                        manifest.display()
                    ),
                });
            }
            if previous == Some(manifest) {
                return Err(ConfigError::InvalidField {
                    field: "release.auxiliary_cargo_manifests".to_string(),
                    reason: format!("duplicate manifest '{}'", manifest.display()),
                });
            }
            previous = Some(manifest);
        }
        Ok(())
    }

    fn validate_version_groups(&self, workspace_members: &[String]) -> Result<(), ConfigError> {
        let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
        for (group_name, members) in &self.version_groups {
            if group_name.trim().is_empty() {
                return Err(ConfigError::InvalidField {
                    field: "release.version_groups".to_string(),
                    reason: "version group names cannot be empty".to_string(),
                });
            }
            if members.is_empty() {
                return Err(ConfigError::InvalidField {
                    field: format!("release.version_groups.{}", group_name),
                    reason: "version groups must contain at least one crate".to_string(),
                });
            }
            for crate_name in members {
                if !workspace_members.contains(crate_name) {
                    return Err(ConfigError::InvalidField {
                        field: format!("release.version_groups.{}", group_name),
                        reason: format!("unknown workspace crate '{}'", crate_name),
                    });
                }
                if let Some(previous) = owners.insert(crate_name, group_name) {
                    return Err(ConfigError::InvalidField {
                        field: "release.version_groups".to_string(),
                        reason: format!(
                            "crate '{}' belongs to multiple version groups: {}, {}",
                            crate_name, previous, group_name
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// How `--bump auto` maps breaking changes on pre-1.0 versions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pre1BreakingBump {
    /// Breaking change on 0.x bumps minor (semver-compatible for 0.x)
    #[default]
    Minor,
    /// Breaking change on 0.x bumps major (graduates to 1.0.0)
    Major,
}

/// Policy for cargo-semver-checks integration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SemverCheckPolicy {
    /// Never run cargo-semver-checks
    Off,
    /// Validate planned intent and report extended-check findings (default)
    #[default]
    Warn,
    /// Fail extended checks when detected API breakage exists
    Deny,
}

/// Remote effects authorized after local release preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseRemoteEffects {
    /// Keep release effects local.
    #[default]
    None,
    /// Push the release commit and tags without creating a forge release.
    Push,
    /// Push and detect the forge provider from `origin`.
    Auto,
    /// Push and create a GitHub release via `gh`.
    Github,
    /// Push and create a GitLab release via `glab`.
    Gitlab,
}

/// Registry publication authority for release execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseRegistryPublication {
    /// Do not authorize publication to any package registry.
    #[default]
    None,
    /// Permit a matching `--publish` invocation to publish to crates.io.
    CratesIo,
}

impl ReleaseRegistryPublication {
    /// Exact registry identity authorized by this policy.
    pub const fn registry(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::CratesIo => Some("crates-io"),
        }
    }
}

impl ReleaseRemoteEffects {
    /// Stable configuration spelling used in durable release trailers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Push => "push",
            Self::Auto => "auto",
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }

    /// Whether the policy authorizes pushing the release commit and tags.
    pub const fn pushes(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether the policy authorizes creating a forge release.
    pub const fn creates_forge_release(self) -> bool {
        matches!(self, Self::Auto | Self::Github | Self::Gitlab)
    }
}

/// Changelog location and shape (`[release.changelog]`)
///
/// Declarative: sections, ordering, and entry rendering are configured with
/// placeholder templates, never a template engine. All keys have per-crate
/// overrides in `[crates.NAME.changelog]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogShape {
    /// Changelog file path (default: "CHANGELOG.md")
    #[serde(default = "default_changelog_path")]
    pub path: String,

    /// What changelog paths are relative to (default: "crate")
    /// - "crate": relative to each crate's directory
    /// - "workspace": relative to the workspace root
    #[serde(default)]
    pub relative_to: ChangelogRelativeTo,
}

impl Default for ChangelogShape {
    fn default() -> Self {
        Self {
            path: default_changelog_path(),
            relative_to: ChangelogRelativeTo::default(),
        }
    }
}

/// Release configuration for a crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateReleaseConfig {
    /// Enable or disable publishing for this crate within Cargo's manifest authority.
    #[serde(default = "default_true")]
    pub publish: bool,
}

/// Per-crate changelog configuration (`[crates.NAME.changelog]`)
///
/// Every field is optional; absent fields inherit `[release.changelog]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangelogConfig {
    /// Path to changelog file
    /// Relative to crate directory (default) or workspace root depending on `relative_to`
    pub path: Option<PathBuf>,
    /// Override what `path` is relative to for this crate
    pub relative_to: Option<ChangelogRelativeTo>,
    /// Exclude this crate from changelog generation?
    #[serde(default)]
    pub skip: bool,
}

/// What the changelog path is relative to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangelogRelativeTo {
    /// Relative to each crate's directory (default)
    /// With this, `path = "CHANGELOG.md"` creates `crates/foo/CHANGELOG.md`
    #[default]
    Crate,
    /// Relative to workspace root
    /// With this, `path = "CHANGELOG.md"` creates `./CHANGELOG.md`
    Workspace,
}

// Default Functions

fn default_tag_prefix() -> String {
    "v".to_string()
}

fn default_tag_format() -> String {
    // Use {prefix} placeholder so tag_prefix is respected
    // With default tag_prefix="v", this produces: crate-name-v1.0.0
    "{crate}-{prefix}{version}".to_string()
}

fn default_changelog_path() -> String {
    "CHANGELOG.md".to_string()
}

fn default_change_dir() -> String {
    crate::release::change_files::DEFAULT_CHANGE_DIR.to_string()
}

fn validate_change_dir(change_dir: &str) -> Result<(), ConfigError> {
    if let Some(reason) = crate::release::change_files::change_dir_validation_error(change_dir) {
        return Err(ConfigError::InvalidField {
            field: "release.change_dir".to_string(),
            reason: reason.to_string(),
        });
    }
    Ok(())
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_shape_defaults() {
        let shape = ChangelogShape::default();
        assert_eq!(shape.path, "CHANGELOG.md");
        assert_eq!(shape.relative_to, ChangelogRelativeTo::Crate);
    }

    #[test]
    fn changelog_shape_parses_nested_table() {
        let toml = r#"
      [changelog]
      path = "docs/CHANGELOG.md"
      relative_to = "workspace"
    "#;
        let config: ReleaseConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.changelog.path, "docs/CHANGELOG.md");
        assert_eq!(config.changelog.relative_to, ChangelogRelativeTo::Workspace);
    }

    #[test]
    fn release_policy_defaults() {
        let config = ReleaseConfig::default();
        assert_eq!(config.pre_1_breaking_bump, Pre1BreakingBump::Minor);
        assert_eq!(config.semver_check, SemverCheckPolicy::Warn);
        assert_eq!(config.remote_effects, ReleaseRemoteEffects::None);
        assert_eq!(config.registry_publication, ReleaseRegistryPublication::None);
        assert_eq!(config.change_dir, ".changes");
        assert!(config.version_groups.is_empty());
        assert!(config.auxiliary_cargo_manifests.is_empty());
    }

    #[test]
    fn release_policies_parse() {
        let toml = r#"
      pre_1_breaking_bump = "major"
      semver_check = "off"
      change_dir = "changes"
      remote_effects = "gitlab"
      registry_publication = "crates-io"
    "#;
        let config: ReleaseConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.pre_1_breaking_bump, Pre1BreakingBump::Major);
        assert_eq!(config.semver_check, SemverCheckPolicy::Off);
        assert_eq!(config.change_dir, "changes");
        assert_eq!(config.remote_effects, ReleaseRemoteEffects::Gitlab);
        assert_eq!(config.registry_publication, ReleaseRegistryPublication::CratesIo);
    }

    #[test]
    fn crate_changelog_overrides_parse() {
        let toml = r#"
      path = "HISTORY.md"
      relative_to = "workspace"
      skip = true
    "#;
        let config: ChangelogConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.path, Some(PathBuf::from("HISTORY.md")));
        assert_eq!(config.relative_to, Some(ChangelogRelativeTo::Workspace));
        assert!(config.skip);
    }

    #[test]
    fn test_version_groups_validate_unknown_and_duplicate_members() {
        let mut config = ReleaseConfig::default();
        config
            .version_groups
            .insert("core".to_string(), vec!["real".to_string(), "ghost".to_string()]);
        let err = config.validate(&["real".to_string()]).unwrap_err();
        assert!(err.to_string().contains("ghost"));

        let mut config = ReleaseConfig::default();
        config
            .version_groups
            .insert("core".to_string(), vec!["real".to_string()]);
        config
            .version_groups
            .insert("cli".to_string(), vec!["real".to_string()]);
        let err = config.validate(&["real".to_string()]).unwrap_err();
        assert!(err.to_string().contains("multiple version groups"));
    }

    #[test]
    fn auxiliary_cargo_manifests_are_exact_relative_manifests() {
        let config: ReleaseConfig =
            toml_edit::de::from_str("auxiliary_cargo_manifests = [\"fuzz/Cargo.toml\", \"tools/check/Cargo.toml\"]")
                .unwrap();
        assert_eq!(config.auxiliary_cargo_manifests.len(), 2);
        config.validate(&["real".to_string()]).unwrap();

        for invalid in [
            "../fuzz/Cargo.toml",
            "./fuzz/Cargo.toml",
            "fuzz/Manifest.toml",
            "/tmp/Cargo.toml",
        ] {
            let config = ReleaseConfig {
                auxiliary_cargo_manifests: vec![PathBuf::from(invalid)],
                ..ReleaseConfig::default()
            };
            let error = config.validate(&["real".to_string()]).unwrap_err();
            assert!(error.to_string().contains("workspace-relative Cargo.toml"), "{error}");
        }

        let config = ReleaseConfig {
            auxiliary_cargo_manifests: vec![PathBuf::from("fuzz/Cargo.toml"); 2],
            ..ReleaseConfig::default()
        };
        let error = config.validate(&["real".to_string()]).unwrap_err();
        assert!(error.to_string().contains("duplicate manifest"), "{error}");
    }
}
