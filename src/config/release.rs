//! Release configuration: which crates a release manages and where its effects go.

use crate::config::unify::default_true;
use crate::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

/// Release configuration (workspace-wide defaults)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseConfig {
    /// Crates a release manages (default: every workspace member).
    ///
    /// `change check` requires change-file coverage only for these crates,
    /// so tooling crates that are never released need no change files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crates: Option<Vec<String>>,

    /// Authoritative source for release bump selection and changelog prose.
    #[serde(default)]
    pub source: ReleaseSource,

    /// Handling for commits that do not parse as conventional (default: "warn").
    ///
    /// Ignored when `source = "changes"`.
    /// - "allow": ignore silently
    /// - "warn": report in the release preview
    /// - "deny": stop the release when its range contains them
    #[serde(default)]
    pub unconventional_commits: CommitPolicy,

    /// Tag format template (default: "{crate}-v{version}").
    /// Variables: {crate}, {version}
    #[serde(default = "default_tag_format")]
    pub tag_format: String,

    /// Package registry that receives releases (default: "crates-io").
    ///
    /// "none" keeps releases to Git tags and GitHub releases. Any other value names a
    /// Cargo-configured registry with a sparse index. Each crate's own `package.publish`
    /// still decides whether it may publish there.
    #[serde(default)]
    pub publish: ReleasePublish,

    /// Create a GitHub release for each tag (default: true when `origin` is on GitHub).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_release: Option<bool>,

    /// What must be green on the source commit before the first release effect
    /// (default: "all" when `origin` is on GitHub, otherwise "none").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<ReleaseChecks>,

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
    /// tool, never vendored. Comparisons use the prior release commit and default features.
    ///
    /// - "off": never run
    /// - "warn": reject incompatible reviewed intent; report unavailable evidence
    /// - "deny": also block when required evidence is unavailable
    #[serde(default)]
    pub semver_check: SemverCheckPolicy,

    /// Complete GitHub release asset inventories, keyed by crate.
    ///
    /// A crate with assets gets a draft release; it is published, and the crate uploaded to its
    /// registry, only after every listed file is attached to the draft.
    /// Names may use `{crate}` and `{version}`.
    #[serde(default)]
    pub assets: BTreeMap<String, Vec<String>>,

    /// Whether each asset must carry a GitHub artifact attestation from this repository's
    /// workflows, made from the release commit (default: "required").
    ///
    /// "off" accepts unattested assets, for private repositories whose GitHub plan cannot attest.
    #[serde(default)]
    pub asset_attestations: AssetAttestations,

    /// Mutable tag aliases moved to a crate's release after its GitHub release is published.
    ///
    /// Values may use `{major}` and `{minor}`, such as `"v{major}"`.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,

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
            crates: None,
            source: ReleaseSource::default(),
            unconventional_commits: CommitPolicy::default(),
            tag_format: default_tag_format(),
            publish: ReleasePublish::default(),
            github_release: None,
            checks: None,
            sign_tags: false,
            change_dir: default_change_dir(),
            pre_1_breaking_bump: Pre1BreakingBump::default(),
            semver_check: SemverCheckPolicy::default(),
            assets: BTreeMap::new(),
            asset_attestations: AssetAttestations::default(),
            aliases: BTreeMap::new(),
            version_groups: BTreeMap::new(),
            auxiliary_cargo_manifests: Vec::new(),
            changelog: ChangelogShape::default(),
        }
    }
}

/// The package registry that receives releases.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ReleasePublish {
    /// crates.io.
    #[default]
    CratesIo,
    /// No registry; releases are Git tags and GitHub releases.
    None,
    /// A registry configured in Cargo under `[registries.NAME]`.
    Registry(String),
}

impl From<String> for ReleasePublish {
    fn from(value: String) -> Self {
        match value.as_str() {
            "crates-io" => Self::CratesIo,
            "none" => Self::None,
            _ => Self::Registry(value),
        }
    }
}

impl From<ReleasePublish> for String {
    fn from(value: ReleasePublish) -> Self {
        match value {
            ReleasePublish::CratesIo => "crates-io".to_owned(),
            ReleasePublish::None => "none".to_owned(),
            ReleasePublish::Registry(name) => name,
        }
    }
}

impl ReleasePublish {
    /// The Cargo registry name, or `None` when releases do not publish.
    pub fn registry(&self) -> Option<&str> {
        match self {
            Self::CratesIo => Some("crates-io"),
            Self::None => None,
            Self::Registry(name) => Some(name),
        }
    }
}

/// Whether release assets must prove where they were built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetAttestations {
    /// Every asset carries an attestation from this repository's workflow at the release commit.
    #[default]
    Required,
    /// Assets need only be present and intact.
    Off,
}

/// Which checks gate the first release effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseChecks {
    /// Every check run and commit status GitHub reports on the source commit must succeed.
    All,
    /// No gate.
    None,
}

/// Authoritative input used to select releases and render changelogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseSource {
    /// Use reviewed change files. Commit messages are not release inputs.
    #[default]
    Changes,
    /// Derive release intent from conventional commits.
    Commits,
    /// Combine reviewed change files and conventional commits.
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

/// Placeholders a tag format may use.
const TAG_FORMAT_PLACEHOLDERS: &[&str] = &["{crate}", "{version}"];
/// Placeholders an alias may use.
const ALIAS_PLACEHOLDERS: &[&str] = &["{major}", "{minor}"];
/// Upper bound on one crate's GitHub release asset inventory.
const MAX_ASSETS: usize = 64;

impl ReleaseConfig {
    /// Whether `crate_name` is a crate this release manages.
    pub fn is_release_crate(&self, crate_name: &str) -> bool {
        self.crates
            .as_ref()
            .is_none_or(|crates| crates.iter().any(|name| name == crate_name))
    }

    /// Whether code changes in this crate require reviewed coverage.
    pub fn requires_change_file(&self, crate_name: &str) -> bool {
        self.source == ReleaseSource::Changes && self.is_release_crate(crate_name)
    }

    /// Whether this release creates GitHub releases for an origin on (or off) GitHub.
    pub fn creates_github_release(&self, github_origin: bool) -> Result<bool, ConfigError> {
        match self.github_release {
            Some(true) if !github_origin => Err(ConfigError::InvalidField {
                field: "release.github_release".to_owned(),
                reason: "origin is not a GitHub repository".to_owned(),
            }),
            Some(enabled) => Ok(enabled),
            None => Ok(github_origin),
        }
    }

    /// The effective checks gate for an origin on (or off) GitHub.
    pub fn effective_checks(&self, github_origin: bool) -> Result<ReleaseChecks, ConfigError> {
        match self.checks {
            Some(ReleaseChecks::All) if !github_origin => Err(ConfigError::InvalidField {
                field: "release.checks".to_owned(),
                reason: "checks are read from GitHub, but origin is not a GitHub repository".to_owned(),
            }),
            Some(checks) => Ok(checks),
            None if github_origin => Ok(ReleaseChecks::All),
            None => Ok(ReleaseChecks::None),
        }
    }

    /// The asset file names configured for `crate_name` at `version`.
    pub fn assets_for(&self, crate_name: &str, version: &semver::Version) -> Vec<String> {
        self.assets.get(crate_name).map_or_else(Vec::new, |files| {
            files
                .iter()
                .map(|file| render_tag_template(file, crate_name, &version.to_string()))
                .collect()
        })
    }

    /// Render the tag alias configured for `crate_name` at `version`.
    pub fn alias_for(&self, crate_name: &str, version: &semver::Version) -> Option<String> {
        self.aliases.get(crate_name).map(|alias| {
            alias
                .replace("{major}", &version.major.to_string())
                .replace("{minor}", &version.minor.to_string())
        })
    }

    /// Validate the release configuration
    pub fn validate(&self, workspace_members: &[String]) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();

        self.validate_policy()?;

        let known = |field: &str, names: Vec<&String>| {
            names
                .into_iter()
                .find(|name| !workspace_members.contains(name))
                .map_or(Ok(()), |name| {
                    Err(ConfigError::InvalidField {
                        field: field.to_owned(),
                        reason: format!("unknown workspace crate '{name}'"),
                    })
                })
        };
        if let Some(crates) = &self.crates {
            known("release.crates", crates.iter().collect())?;
        }
        known("release.assets", self.assets.keys().collect())?;
        known("release.aliases", self.aliases.keys().collect())?;
        if let Some(name) = self.assets.keys().find(|name| !self.is_release_crate(name)) {
            return Err(ConfigError::InvalidField {
                field: "release.assets".to_owned(),
                reason: format!("'{name}' is not listed in release.crates"),
            });
        }
        if let Some(name) = self.aliases.keys().find(|name| !self.is_release_crate(name)) {
            return Err(ConfigError::InvalidField {
                field: "release.aliases".to_owned(),
                reason: format!("'{name}' is not listed in release.crates"),
            });
        }

        // Check for recommended placeholders in monorepo context
        let released = workspace_members
            .iter()
            .filter(|name| self.is_release_crate(name))
            .count();
        if released > 1 && !self.tag_format.contains("{crate}") {
            warnings.push(
                "release.tag_format does not contain {crate}; in a workspace that releases several crates, \
                 two crates at one version would need the same tag"
                    .to_string(),
            );
        }
        self.validate_version_groups(workspace_members)?;

        Ok(warnings)
    }

    pub(crate) fn validate_policy(&self) -> Result<(), ConfigError> {
        validate_tag_format("release.tag_format", &self.tag_format)?;
        if let ReleasePublish::Registry(name) = &self.publish
            && !valid_registry_name(name)
        {
            return Err(ConfigError::InvalidField {
                field: "release.publish".to_owned(),
                reason: format!("'{name}' is not \"crates-io\", \"none\", or a Cargo registry name"),
            });
        }
        validate_change_dir(&self.change_dir)?;
        self.changelog.filters.validate("release.changelog.filters")?;
        self.validate_auxiliary_cargo_manifests()?;
        if let Some(crates) = &self.crates
            && (crates.is_empty() || crates.iter().collect::<std::collections::BTreeSet<_>>().len() != crates.len())
        {
            return Err(ConfigError::InvalidField {
                field: "release.crates".to_owned(),
                reason: "must list at least one crate, each once".to_owned(),
            });
        }
        for (name, files) in &self.assets {
            if files.is_empty()
                || files.len() > MAX_ASSETS
                || files.iter().collect::<std::collections::BTreeSet<_>>().len() != files.len()
                || files
                    .iter()
                    .any(|file| !valid_asset_name(&render_tag_template(file, "x", "1.0.0")))
            {
                return Err(ConfigError::InvalidField {
                    field: format!("release.assets.{name}"),
                    reason: format!(
                        "must list 1 to {MAX_ASSETS} unique file names of letters, digits, '.', '_', '+', or '-'; \
                         {{crate}} and {{version}} are the only placeholders"
                    ),
                });
            }
        }
        let rendered = self
            .aliases
            .values()
            .map(|alias| {
                let mut rendered = alias.clone();
                for placeholder in ALIAS_PLACEHOLDERS {
                    rendered = rendered.replace(placeholder, "0");
                }
                rendered
            })
            .collect::<Vec<_>>();
        if rendered.iter().any(|alias| !valid_tag_name(alias))
            || self.aliases.values().collect::<std::collections::BTreeSet<_>>().len() != self.aliases.len()
        {
            return Err(ConfigError::InvalidField {
                field: "release.aliases".into(),
                reason: "aliases must be unique, bounded, pathless Git tag names; {major} and {minor} are the only placeholders"
                    .into(),
            });
        }
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

/// Replace `{crate}` and `{version}` in a tag or asset template.
pub(crate) fn render_tag_template(template: &str, crate_name: &str, version: &str) -> String {
    let mut rendered = template.to_owned();
    for (placeholder, value) in TAG_FORMAT_PLACEHOLDERS.iter().zip([crate_name, version]) {
        rendered = rendered.replace(placeholder, value);
    }
    rendered
}

/// Validate a tag format: it names `{version}` and uses no other placeholder than `{crate}`.
pub(crate) fn validate_tag_format(field: &str, format: &str) -> Result<(), ConfigError> {
    let mut rest = format.to_owned();
    for placeholder in TAG_FORMAT_PLACEHOLDERS {
        rest = rest.replace(placeholder, "x");
    }
    let reason = if format.trim().is_empty() {
        Some("tag_format cannot be empty")
    } else if format.contains("{prefix}") {
        Some(
            "{prefix} was removed in Cargo-Rail 0.32.0; write the prefix literally, such as \"{crate}-v{version}\"; `cargo rail config migrate` rewrites it",
        )
    } else if !format.contains("{version}") {
        Some("must contain {version}")
    } else if rest.contains(['{', '}']) || !valid_tag_name(&rest) {
        Some("must render a valid Git tag name; the only placeholders are {crate} and {version}")
    } else {
        None
    };
    reason.map_or(Ok(()), |reason| {
        Err(ConfigError::InvalidField {
            field: field.to_owned(),
            reason: reason.to_owned(),
        })
    })
}

/// A bounded Git tag name without path separators, matching what `check-ref-format` accepts here.
fn valid_tag_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && !name.ends_with('.')
        && !name.ends_with(".lock")
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

/// A Cargo registry name: what `[registries.NAME]` accepts.
fn valid_registry_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// A release asset file name GitHub stores unchanged.
fn valid_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
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
    /// Fail the release
    Deny,
}

/// Policy for cargo-semver-checks integration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SemverCheckPolicy {
    /// Never run cargo-semver-checks
    Off,
    /// Validate reviewed intent and report unavailable evidence (default)
    #[default]
    Warn,
    /// Also block release when required API evidence is unavailable
    Deny,
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
    /// Override the global tag format for this crate's release history.
    pub tag_format: Option<String>,
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

fn default_tag_format() -> String {
    "{crate}-v{version}".to_string()
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
        assert_eq!(config.tag_format, "{crate}-v{version}");
        assert_eq!(config.pre_1_breaking_bump, Pre1BreakingBump::Minor);
        assert_eq!(config.semver_check, SemverCheckPolicy::Warn);
        assert_eq!(config.publish, ReleasePublish::CratesIo);
        assert_eq!(config.change_dir, ".changes");
        assert!(config.version_groups.is_empty());
        assert!(config.auxiliary_cargo_manifests.is_empty());
        assert!(config.is_release_crate("anything"));
    }

    #[test]
    fn forge_defaults_follow_the_origin_and_explicit_github_policy_requires_github() {
        let config = ReleaseConfig::default();
        assert!(config.creates_github_release(true).unwrap());
        assert!(!config.creates_github_release(false).unwrap());
        assert_eq!(config.effective_checks(true).unwrap(), ReleaseChecks::All);
        assert_eq!(config.effective_checks(false).unwrap(), ReleaseChecks::None);

        let explicit: ReleaseConfig = toml_edit::de::from_str("github_release = true\nchecks = \"all\"").unwrap();
        explicit.creates_github_release(false).unwrap_err();
        explicit.effective_checks(false).unwrap_err();
        let off: ReleaseConfig = toml_edit::de::from_str("github_release = false\nchecks = \"none\"").unwrap();
        assert!(!off.creates_github_release(true).unwrap());
        assert_eq!(off.effective_checks(true).unwrap(), ReleaseChecks::None);
    }

    #[test]
    fn publish_names_crates_io_none_or_a_cargo_registry() {
        for (value, expected) in [
            ("crates-io", ReleasePublish::CratesIo),
            ("none", ReleasePublish::None),
            ("internal", ReleasePublish::Registry("internal".to_owned())),
        ] {
            let config: ReleaseConfig = toml_edit::de::from_str(&format!("publish = {value:?}")).unwrap();
            assert_eq!(config.publish, expected);
            config.validate_policy().unwrap();
            assert_eq!(String::from(config.publish), value);
        }
        let invalid: ReleaseConfig = toml_edit::de::from_str("publish = \"not a registry\"").unwrap();
        assert!(invalid.validate_policy().is_err());
    }

    #[test]
    fn tag_formats_use_only_crate_and_version_and_retire_prefix() {
        for valid in ["{crate}-v{version}", "v{version}", "release-{version}"] {
            validate_tag_format("release.tag_format", valid).unwrap();
        }
        let retired = concat!("{crate}-{", "prefix}{version}");
        let prefix = validate_tag_format("release.tag_format", retired).unwrap_err();
        assert!(prefix.to_string().contains("config migrate"), "{prefix}");
        for invalid in ["{crate}", "{crate}-{date}-{version}", "../{version}", "v {version}"] {
            assert!(validate_tag_format("release.tag_format", invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn aliases_render_major_and_minor() {
        let config: ReleaseConfig =
            toml_edit::de::from_str("[aliases]\ncargo-rail-action = \"v{major}\"\nlib = \"v{major}.{minor}\"").unwrap();
        config.validate_policy().unwrap();
        let version = semver::Version::new(12, 3, 1);
        assert_eq!(config.alias_for("cargo-rail-action", &version).as_deref(), Some("v12"));
        assert_eq!(config.alias_for("lib", &version).as_deref(), Some("v12.3"));
        assert_eq!(config.alias_for("other", &version), None);

        let unknown: ReleaseConfig = toml_edit::de::from_str("[aliases]\nlib = \"v{patch}\"").unwrap();
        assert!(unknown.validate_policy().is_err());
    }

    #[test]
    fn release_crates_scope_change_coverage_assets_and_aliases() {
        let members = ["app".to_owned(), "xtask".to_owned()];
        let config: ReleaseConfig = toml_edit::de::from_str("crates = [\"app\"]").unwrap();
        config.validate(&members).unwrap();
        assert!(config.requires_change_file("app"));
        assert!(!config.requires_change_file("xtask"));

        let unknown: ReleaseConfig = toml_edit::de::from_str("crates = [\"ghost\"]").unwrap();
        unknown.validate(&members).unwrap_err();
        let unmanaged: ReleaseConfig =
            toml_edit::de::from_str("crates = [\"app\"]\n[assets]\nxtask = [\"xtask.zip\"]").unwrap();
        let error = unmanaged.validate(&members).unwrap_err();
        assert!(error.to_string().contains("release.crates"), "{error}");
        let empty: ReleaseConfig = toml_edit::de::from_str("crates = []").unwrap();
        assert!(empty.validate_policy().is_err());
    }

    #[test]
    fn asset_inventories_are_unique_flat_file_names() {
        let valid: ReleaseConfig =
            toml_edit::de::from_str("[assets]\napp = [\"app-x86_64-unknown-linux-gnu.zip\", \"SHA256SUMS\"]").unwrap();
        valid.validate_policy().unwrap();
        for invalid in ["[]", "[\"dir/app.zip\"]", "[\".hidden\"]", "[\"a.zip\", \"a.zip\"]"] {
            let config: ReleaseConfig = toml_edit::de::from_str(&format!("[assets]\napp = {invalid}")).unwrap();
            assert!(config.validate_policy().is_err(), "{invalid}");
        }
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
