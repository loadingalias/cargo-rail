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
    /// Require change files to cover released crates in commit workflows.
    ///
    /// Changes mode always requires repository-wide coverage. In commit
    /// workflows:
    /// - `false` (default): change files are optional
    /// - `true`: every released crate with code changes needs a covering change file
    /// - `["crate-a", ...]`: only the listed crates are gated
    #[serde(default)]
    pub require_change_files: RequireChangeFiles,

    /// Handling for commits that do not parse as conventional (default: "warn").
    ///
    /// Ignored when `source = "changes"`.
    /// - "allow": ignore silently
    /// - "warn": report in `release check` and plan output
    /// - "deny": fail `release check` when the release range contains them
    #[serde(default)]
    pub unconventional_commits: CommitPolicy,

    /// Directory containing manual release note overrides.
    ///
    /// If `release-notes/v1.2.3.md` or `release-notes/<tag>.md` exists, cargo-rail
    /// uses it as the forge release body instead of generated changelog text.
    #[serde(default = "default_release_notes_dir")]
    pub release_notes_dir: String,

    /// If true, require release notes for the target version before publishing/tagging.
    ///
    /// This preflight check fails release apply when generated changelog entries are empty
    /// and no existing `## [<version>]` section exists in the crate's changelog.
    #[serde(default = "default_true")]
    pub require_release_notes: bool,

    /// If true, error when there are no changelog entries for a crate
    #[serde(default)]
    pub require_changelog_entries: bool,

    /// Authoritative source for release bump selection and changelog prose.
    #[serde(default)]
    pub source: ReleaseSource,

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
            require_change_files: RequireChangeFiles::default(),
            unconventional_commits: CommitPolicy::default(),
            release_notes_dir: default_release_notes_dir(),
            require_release_notes: true,
            require_changelog_entries: false,
            source: ReleaseSource::default(),
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

/// Authoritative input used to select releases and render changelogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseSource {
    /// Use reviewed change files. Commit messages are not release inputs.
    #[default]
    Changes,
    /// Use conventional commits for compatibility with older repositories.
    Commits,
    /// Combine reviewed change files and conventional commits for compatibility.
    Both,
}

impl ReleaseSource {
    /// Whether conventional commits contribute to release planning.
    pub const fn uses_commits(self) -> bool {
        matches!(self, Self::Commits | Self::Both)
    }

    /// Whether reviewed change files contribute to release planning.
    pub const fn uses_changes(self) -> bool {
        matches!(self, Self::Changes | Self::Both)
    }
}

impl ReleaseConfig {
    /// Whether code changes in this crate require reviewed coverage.
    pub fn requires_change_file(&self, crate_name: &str) -> bool {
        self.source == ReleaseSource::Changes || self.require_change_files.applies_to(crate_name)
    }

    /// Validate the release configuration
    pub fn validate(&self, workspace_members: &[String]) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();

        self.validate_policy()?;

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

        if let RequireChangeFiles::Crates(crates) = &self.require_change_files {
            for name in crates {
                if !workspace_members.contains(name) {
                    warnings.push(format!("release.require_change_files contains unknown crate '{name}'"));
                }
            }
        }

        self.validate_version_groups(workspace_members)?;

        Ok(warnings)
    }

    pub(crate) fn validate_policy(&self) -> Result<(), ConfigError> {
        // Validate tag_format
        if self.tag_format.trim().is_empty() {
            return Err(ConfigError::InvalidField {
                field: "release.tag_format".to_string(),
                reason: "tag_format cannot be empty".to_string(),
            });
        }

        validate_change_dir(&self.change_dir)?;
        if let Some(reason) = crate::release::change_files::change_dir_validation_error(&self.release_notes_dir) {
            return Err(ConfigError::InvalidField {
                field: "release.release_notes_dir".to_string(),
                reason: reason.to_string(),
            });
        }
        self.changelog.filters.validate("release.changelog.filters")?;
        self.validate_auxiliary_cargo_manifests()?;
        Ok(())
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

/// Policy for commits that fail conventional-commit parsing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitPolicy {
    /// Ignore silently
    Allow,
    /// Report as diagnostics (default)
    #[default]
    Warn,
    /// Fail `release check`
    Deny,
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

/// Which crates require change-file coverage
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequireChangeFiles {
    /// Gate all crates (true) or none (false)
    All(bool),
    /// Gate only the listed crates
    Crates(Vec<String>),
}

impl Default for RequireChangeFiles {
    fn default() -> Self {
        Self::All(false)
    }
}

impl RequireChangeFiles {
    /// Whether the gate applies to `crate_name`
    pub fn applies_to(&self, crate_name: &str) -> bool {
        match self {
            Self::All(enabled) => *enabled,
            Self::Crates(crates) => crates.iter().any(|c| c == crate_name),
        }
    }

    /// Whether the gate applies to any crate at all
    pub fn is_enabled(&self) -> bool {
        match self {
            Self::All(enabled) => *enabled,
            Self::Crates(crates) => !crates.is_empty(),
        }
    }
}

/// Changelog location and shape (`[release.changelog]`)
///
/// Declarative: sections, ordering, and entry rendering are configured with
/// placeholder templates, never a template engine. All keys have per-crate
/// overrides in `[crates.NAME.changelog]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangelogShape {
    /// Changelog file path (default: "CHANGELOG.md")
    #[serde(default = "default_changelog_path")]
    pub path: String,

    /// What changelog paths are relative to (default: "crate")
    /// - "crate": relative to each crate's directory
    /// - "workspace": relative to the workspace root
    #[serde(default)]
    pub relative_to: ChangelogRelativeTo,

    /// Entry line template. Placeholders render with their separators and
    /// collapse to nothing when empty:
    /// - {scope}: "**scope**: " when the commit has a scope
    /// - {breaking}: "\[**breaking**\] " for breaking changes
    /// - {description}: the commit description
    /// - {prs}: " #12 #34" (linked when a PR URL is known)
    /// - {sha}: short commit SHA
    /// - {sha_link}: short SHA linked to the commit when a commit URL is known
    /// - {type}: the parsed commit type
    #[serde(default = "default_entry_format")]
    pub entry_format: String,

    /// Render emoji in section headers (default: true)
    #[serde(default = "default_true")]
    pub emoji: bool,

    /// Section render order, by commit type. Types absent from this list fall
    /// through to `fallback`. Breaking commits always classify as "breaking".
    #[serde(default = "default_group_order")]
    pub group_order: Vec<String>,

    /// Where unlisted or unknown commit types go: a type key from
    /// `group_order`, or "skip" to drop them (default: "other")
    #[serde(default = "default_fallback")]
    pub fallback: String,

    /// Custom commit types and section overrides. Each entry maps one or more
    /// commit types to a shared section.
    #[serde(default)]
    pub groups: Vec<GroupSpec>,

    /// Commit filters applied before grouping
    #[serde(default)]
    pub filters: ChangelogFilters,

    /// Commit link template with a {sha} placeholder
    /// (default: inferred from a GitHub `origin` remote)
    #[serde(default)]
    pub commit_url: Option<String>,

    /// Pull-request link template with a {pr} placeholder
    /// (default: inferred from a GitHub `origin` remote)
    #[serde(default)]
    pub pr_url: Option<String>,
}

impl Default for ChangelogShape {
    fn default() -> Self {
        Self {
            path: default_changelog_path(),
            relative_to: ChangelogRelativeTo::default(),
            entry_format: default_entry_format(),
            emoji: true,
            group_order: default_group_order(),
            fallback: default_fallback(),
            groups: Vec::new(),
            filters: ChangelogFilters::default(),
            commit_url: None,
            pr_url: None,
        }
    }
}

/// A custom changelog section: one or more commit types rendered together
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    /// Commit types that map to this section (e.g. ["sec", "security"])
    pub types: Vec<String>,
    /// Section title (e.g. "Security")
    pub title: String,
    /// Section emoji (omit to inherit none)
    #[serde(default)]
    pub emoji: Option<String>,
}

/// Commit filters applied before grouping
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangelogFilters {
    /// Commit types to drop entirely (e.g. ["chore", "ci"])
    #[serde(default)]
    pub skip_types: Vec<String>,
    /// Commit scopes to drop entirely (e.g. ["internal"])
    #[serde(default)]
    pub skip_scopes: Vec<String>,
    /// If non-empty, only commits touching paths matching one of these glob
    /// patterns are attributed to changelogs.
    #[serde(default)]
    pub include_paths: Vec<String>,
    /// Commits touching only paths matching these glob patterns are excluded
    /// from changelog attribution.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
}

impl ChangelogFilters {
    /// Validate glob path filters.
    pub fn validate(&self, field_prefix: &str) -> Result<(), ConfigError> {
        validate_globs(field_prefix, "include_paths", &self.include_paths)?;
        validate_globs(field_prefix, "exclude_paths", &self.exclude_paths)
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
#[serde(deny_unknown_fields)]
pub struct ChangelogConfig {
    /// Path to changelog file
    /// Relative to crate directory (default) or workspace root depending on `relative_to`
    pub path: Option<PathBuf>,
    /// Override what `path` is relative to for this crate
    pub relative_to: Option<ChangelogRelativeTo>,
    /// Exclude this crate from changelog generation?
    #[serde(default)]
    pub skip: bool,
    /// Override the entry line template for this crate
    pub entry_format: Option<String>,
    /// Override emoji rendering for this crate
    pub emoji: Option<bool>,
    /// Override the section order for this crate
    pub group_order: Option<Vec<String>>,
    /// Override the fallback section for this crate
    pub fallback: Option<String>,
    /// Additional custom sections for this crate (extend workspace groups)
    #[serde(default)]
    pub groups: Vec<GroupSpec>,
    /// Override commit filters for this crate
    pub filters: Option<ChangelogFilters>,
    /// Override commit link template with a {sha} placeholder
    pub commit_url: Option<String>,
    /// Override pull-request link template with a {pr} placeholder
    pub pr_url: Option<String>,
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

fn default_release_notes_dir() -> String {
    "release-notes".to_string()
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

fn validate_globs(field_prefix: &str, key: &str, patterns: &[String]) -> Result<(), ConfigError> {
    for pattern in patterns {
        if let Err(e) = glob::Pattern::new(pattern) {
            return Err(ConfigError::InvalidGlobPattern {
                pattern: format!("{}.{} = {}", field_prefix, key, pattern),
                message: e.to_string(),
            });
        }
    }
    Ok(())
}

fn default_entry_format() -> String {
    "- {scope}{breaking}{description}{prs} ({sha_link})".to_string()
}

fn default_fallback() -> String {
    "other".to_string()
}

/// Default section order: breaking first, features and fixes next, remaining
/// built-in types alphabetically (matches deterministic render order).
pub(crate) fn default_group_order() -> Vec<String> {
    [
        "breaking", "feat", "fix", "build", "chore", "ci", "deps", "docs", "other", "perf", "refactor", "style", "test",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
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
