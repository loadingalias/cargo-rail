//! Lossless normalization for the exact v0.25.0 `rail.toml` contract.
//!
//! Configuration owns this predecessor boundary. Keep it while v0.25.0 is
//! the supported upgrade baseline; remove it only when a later release moves
//! that baseline and no supported planner comparison or migration needs it.

use super::{RailConfig, schema};
use crate::error::{RailError, RailResult};
use std::path::{Component, Path};
use toml_edit::{Array, DocumentMut, InlineTable, Item, TableLike, Value};

/// One explicit v0.25.0-to-current normalization.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct MigrationChange {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) replacement: Option<String>,
    pub(crate) message: &'static str,
}

/// A validated current configuration and its losslessly normalized TOML.
pub(crate) struct NormalizedConfig {
    pub(crate) config: RailConfig,
    pub(crate) bytes: Vec<u8>,
    pub(crate) changes: Vec<MigrationChange>,
}

/// Typed portion of the exact v0.25.0 input that is removed before current
/// deserialization. Fields retained by the current contract are validated by
/// `RailConfig` after normalization; implementation-only predecessor fields
/// were intentionally ignored by v0.25.0 and remain untyped here.
#[derive(serde::Deserialize, Default)]
struct V0_25CompatibilityInput {
    #[serde(default)]
    unify: V0_25UnifyInput,
    #[serde(default)]
    release: V0_25ReleaseInput,
    #[serde(default, rename = "crates")]
    _crates: std::collections::BTreeMap<String, V0_25CrateInput>,
}

#[derive(serde::Deserialize, Default)]
struct V0_25UnifyInput {
    transitive_pinning: Option<serde::de::IgnoredAny>,
    pin_transitives: Option<bool>,
    transitive_host: Option<String>,
    msrv_policy: Option<serde::de::IgnoredAny>,
    msrv: Option<bool>,
    enforce_msrv_inheritance: Option<bool>,
    msrv_source: Option<V0_25MsrvSource>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum V0_25MsrvSource {
    Deps,
    Workspace,
    Max,
}

#[derive(serde::Deserialize, Default)]
struct V0_25ReleaseInput {
    #[serde(rename = "source")]
    _source: Option<V0_25ReleaseSource>,
    #[serde(rename = "require_clean")]
    _require_clean: Option<bool>,
    #[serde(rename = "publish_delay")]
    _publish_delay: Option<u64>,
    remote_effects: Option<serde::de::IgnoredAny>,
    create_github_release: Option<bool>,
    forge: Option<V0_25ReleaseForge>,
    push: Option<bool>,
    #[serde(rename = "require_changelog_entries")]
    _require_changelog_entries: Option<bool>,
    #[serde(rename = "require_release_notes")]
    _require_release_notes: Option<bool>,
    #[serde(rename = "release_notes_dir")]
    _release_notes_dir: Option<std::path::PathBuf>,
    #[serde(rename = "unconventional_commits")]
    _unconventional_commits: Option<V0_25CommitPolicy>,
    #[serde(rename = "require_change_files")]
    _require_change_files: Option<V0_25RequireChangeFiles>,
    #[serde(rename = "changelog")]
    _changelog: Option<V0_25ChangelogShape>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum V0_25ReleaseSource {
    Changes,
    Commits,
    Both,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum V0_25ReleaseForge {
    Auto,
    Github,
    Gitlab,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum V0_25CommitPolicy {
    Allow,
    Warn,
    Deny,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
#[expect(
    dead_code,
    reason = "variant payloads are deserialized only to validate the exact v0.25.0 input type"
)]
enum V0_25RequireChangeFiles {
    All(bool),
    Crates(Vec<String>),
}

#[derive(serde::Deserialize)]
struct V0_25ChangelogShape {
    #[serde(rename = "entry_format")]
    _entry_format: Option<String>,
    #[serde(rename = "emoji")]
    _emoji: Option<bool>,
    #[serde(rename = "group_order")]
    _group_order: Option<Vec<String>>,
    #[serde(rename = "fallback")]
    _fallback: Option<String>,
    #[serde(rename = "groups")]
    _groups: Option<Vec<V0_25GroupSpec>>,
    #[serde(rename = "filters")]
    _filters: Option<V0_25ChangelogFilters>,
    #[serde(rename = "commit_url")]
    _commit_url: Option<String>,
    #[serde(rename = "pr_url")]
    _pr_url: Option<String>,
}

#[derive(serde::Deserialize)]
struct V0_25GroupSpec {
    #[serde(rename = "types")]
    _types: Vec<String>,
    #[serde(rename = "title")]
    _title: String,
    #[serde(rename = "emoji")]
    _emoji: Option<String>,
}

#[derive(serde::Deserialize)]
struct V0_25ChangelogFilters {
    #[serde(rename = "skip_types")]
    _skip_types: Option<Vec<String>>,
    #[serde(rename = "skip_scopes")]
    _skip_scopes: Option<Vec<String>>,
    #[serde(rename = "include_paths")]
    _include_paths: Option<Vec<String>>,
    #[serde(rename = "exclude_paths")]
    _exclude_paths: Option<Vec<String>>,
}

#[derive(serde::Deserialize, Default)]
struct V0_25CrateInput {
    #[serde(rename = "split")]
    _split: Option<V0_25SplitInput>,
    #[serde(rename = "changelog")]
    _changelog: Option<V0_25ChangelogShape>,
}

#[derive(serde::Deserialize)]
struct V0_25SplitInput {
    #[serde(rename = "paths")]
    _paths: Option<Vec<V0_25CratePath>>,
}

#[derive(serde::Deserialize)]
struct V0_25CratePath {
    #[serde(rename = "crate")]
    _path: std::path::PathBuf,
}

// This is the exact set of v0.25.0 schema leaves that no longer exist in the
// current schema. Current fields remain owned by `schema::FIELD_SPECS`.
const RETIRED_V0_25_FIELDS: &[&str] = &[
    "workspace",
    "toolchain",
    "unify.pin_transitives",
    "unify.transitive_host",
    "unify.msrv",
    "unify.enforce_msrv_inheritance",
    "unify.msrv_source",
    "unify.prune_dead_features",
    "unify.detect_unused",
    "unify.compiler_diag_cache",
    "unify.remove_unused",
    "unify.detect_undeclared_features",
    "unify.fix_undeclared_features",
    "unify.sort_dependencies",
    "release.source",
    "release.require_clean",
    "release.publish_delay",
    "release.create_github_release",
    "release.forge",
    "release.push",
    "release.require_changelog_entries",
    "release.require_release_notes",
    "release.release_notes_dir",
    "release.unconventional_commits",
    "release.require_change_files",
    "release.changelog.entry_format",
    "release.changelog.emoji",
    "release.changelog.group_order",
    "release.changelog.fallback",
    "release.changelog.groups",
    "release.changelog.groups.<index>.types",
    "release.changelog.groups.<index>.title",
    "release.changelog.groups.<index>.emoji",
    "release.changelog.filters.skip_types",
    "release.changelog.filters.skip_scopes",
    "release.changelog.filters.include_paths",
    "release.changelog.filters.exclude_paths",
    "release.changelog.commit_url",
    "release.changelog.pr_url",
    "crates.<name>.split.paths",
    "crates.<name>.changelog.entry_format",
    "crates.<name>.changelog.emoji",
    "crates.<name>.changelog.group_order",
    "crates.<name>.changelog.fallback",
    "crates.<name>.changelog.groups",
    "crates.<name>.changelog.groups.<index>.types",
    "crates.<name>.changelog.groups.<index>.title",
    "crates.<name>.changelog.groups.<index>.emoji",
    "crates.<name>.changelog.filters.skip_types",
    "crates.<name>.changelog.filters.skip_scopes",
    "crates.<name>.changelog.filters.include_paths",
    "crates.<name>.changelog.filters.exclude_paths",
    "crates.<name>.changelog.commit_url",
    "crates.<name>.changelog.pr_url",
    "crates.<name>.sync",
];

// v0.25.0 deliberately admitted descendants below these deprecated fields.
// Active v0.25 fields that are removed now still retain their exact old leaf
// inventory and do not gain this looser rule.
const V0_25_DESCENDANT_FIELDS: &[&str] = &[
    "workspace",
    "toolchain",
    "unify.pin_transitives",
    "unify.transitive_host",
    "unify.msrv",
    "unify.enforce_msrv_inheritance",
    "unify.msrv_source",
    "unify.prune_dead_features",
    "unify.detect_unused",
    "unify.compiler_diag_cache",
    "unify.remove_unused",
    "unify.detect_undeclared_features",
    "unify.fix_undeclared_features",
    "unify.sort_dependencies",
    "release.require_clean",
    "release.publish_delay",
    "release.create_github_release",
    "release.forge",
    "release.push",
    "crates.<name>.split.paths",
    "crates.<name>.sync",
];

const REMOVED_FIELDS: &[(&str, &str)] = &[
    (
        "unify.compiler_diag_cache",
        "Compiler evidence caching is now automatic.",
    ),
    (
        "unify.sort_dependencies",
        "Dependency edits are now always deterministic.",
    ),
    (
        "unify.prune_dead_features",
        "Dead-feature diagnostics are unconditional; deletion still requires closed-consumer proof.",
    ),
    (
        "unify.detect_unused",
        "Unused-dependency diagnostics are now unconditional.",
    ),
    (
        "unify.remove_unused",
        "Read-only checks and explicit apply now define the mutation boundary.",
    ),
    (
        "unify.detect_undeclared_features",
        "Borrowed-feature diagnostics are now unconditional.",
    ),
    (
        "unify.fix_undeclared_features",
        "Read-only checks and explicit apply now define the mutation boundary.",
    ),
    (
        "release.source",
        "Reviewed change intent is now the only release input authority.",
    ),
    (
        "release.require_clean",
        "Release cleanliness is enforced by fixed preview/apply semantics.",
    ),
    (
        "release.publish_delay",
        "Registry convergence is an explicit stop-and-resume boundary.",
    ),
    (
        "release.require_changelog_entries",
        "Reviewed change intent now owns changelog coverage.",
    ),
    (
        "release.require_release_notes",
        "Release-note requirements are no longer repository configuration.",
    ),
    (
        "release.release_notes_dir",
        "Release-note discovery is no longer repository configuration.",
    ),
    (
        "release.unconventional_commits",
        "Reviewed change intent replaced commit-message compatibility policy.",
    ),
    (
        "release.require_change_files",
        "Reviewed change intent is now required by the release boundary.",
    ),
    ("workspace", "The reserved workspace table had no behavior."),
    ("toolchain", "The reserved toolchain table had no behavior."),
];

const CHANGELOG_REMOVALS: &[(&str, &str)] = &[
    ("entry_format", "Changelog rendering shape is now code-owned."),
    ("emoji", "Changelog heading decoration is now code-owned."),
    ("group_order", "Changelog section ordering is now code-owned."),
    ("fallback", "Changelog fallback behavior is now code-owned."),
    ("groups", "Changelog section classification is now code-owned."),
    ("filters", "Changelog attribution filtering is now code-owned."),
    ("commit_url", "Commit-link rendering is now code-owned."),
    ("pr_url", "Pull-request-link rendering is now code-owned."),
];

/// Normalize current or exact-v0.25.0 bytes into current planning facts.
///
/// `read_manifest` is called only for legacy split member paths. It receives a
/// validated workspace-relative package directory and must return that
/// directory's `Cargo.toml` bytes from the same captured source boundary.
pub(crate) fn normalize(
    bytes: &[u8],
    mut read_manifest: impl FnMut(&Path) -> RailResult<Vec<u8>>,
) -> RailResult<NormalizedConfig> {
    if let Ok(config) = RailConfig::parse_bytes(bytes) {
        return Ok(NormalizedConfig {
            config,
            bytes: bytes.to_vec(),
            changes: Vec::new(),
        });
    }

    let current_error = RailConfig::parse_bytes(bytes).expect_err("strict current parse already failed");
    let content = std::str::from_utf8(bytes)
        .map_err(|error| RailError::message(format!("configuration is not valid UTF-8: {error}")))?;
    let mut doc: DocumentMut = content
        .parse()
        .map_err(|error: toml_edit::TomlError| RailError::message(error.to_string()))?;
    let paths = schema::document_paths(&doc);
    let has_predecessor_field = paths
        .iter()
        .any(|path| !schema::is_known_config_path(path) && is_retired_v0_25_path(path));
    if !has_predecessor_field {
        return Err(RailError::message(current_error));
    }
    if let Some(path) = v0_25_document_paths(&doc)
        .iter()
        .find(|path| !schema::is_known_config_path(path) && !is_known_v0_25_retired_path(path))
    {
        return Err(RailError::message(format!("unknown configuration key '{path}'")));
    }
    validate_v0_25_typed_input(content)?;

    let mut changes = Vec::new();
    migrate_unify_policies(&mut doc, &mut changes)?;
    migrate_release_remote_effects(&mut doc, &mut changes)?;
    migrate_split_paths(&mut doc, &mut changes, &mut read_manifest)?;
    for (path, message) in REMOVED_FIELDS {
        remove_fixed(&mut doc, &mut changes, path, message);
    }
    remove_changelog_configuration(&mut doc, &mut changes);
    remove_reserved_sync_tables(&mut doc, &mut changes);

    let migrated = doc.to_string().into_bytes();
    let config = RailConfig::parse_bytes(&migrated)
        .map_err(|error| RailError::message(format!("migrated configuration is invalid: {error}")))?;
    Ok(NormalizedConfig {
        config,
        bytes: migrated,
        changes,
    })
}

fn is_retired_v0_25_path(path: &schema::ConfigPath) -> bool {
    RETIRED_V0_25_FIELDS
        .iter()
        .any(|pattern| paths_share_prefix(pattern, path.segments()))
}

fn is_known_v0_25_retired_path(path: &schema::ConfigPath) -> bool {
    RETIRED_V0_25_FIELDS
        .iter()
        .any(|pattern| path_is_prefix_of_pattern(pattern, path.segments()))
        || V0_25_DESCENDANT_FIELDS
            .iter()
            .any(|pattern| pattern_is_prefix_of_path(pattern, path.segments()))
}

fn paths_share_prefix(pattern: &str, actual: &[String]) -> bool {
    let expected = pattern.split('.').collect::<Vec<_>>();
    actual
        .iter()
        .zip(&expected)
        .all(|(actual, expected)| expected.starts_with('<') || actual == expected)
}

fn path_is_prefix_of_pattern(pattern: &str, actual: &[String]) -> bool {
    let expected = pattern.split('.').collect::<Vec<_>>();
    actual.len() <= expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| expected.starts_with('<') || actual == expected)
}

fn pattern_is_prefix_of_path(pattern: &str, actual: &[String]) -> bool {
    let expected = pattern.split('.').collect::<Vec<_>>();
    expected.len() <= actual.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| expected.starts_with('<') || actual == expected)
}

fn v0_25_document_paths(doc: &DocumentMut) -> Vec<schema::ConfigPath> {
    fn collect(table: &toml_edit::Table, prefix: &schema::ConfigPath, paths: &mut Vec<schema::ConfigPath>) {
        for (key, item) in table {
            let path = prefix.child(key);
            paths.push(path.clone());
            if let Some(child) = item.as_table() {
                collect(child, &path, paths);
            } else if let Some(array) = item.as_array_of_tables() {
                for (index, child) in array.iter().enumerate() {
                    collect(child, &path.child(index.to_string()), paths);
                }
            }
        }
    }

    let mut paths = Vec::new();
    collect(doc.as_table(), &schema::ConfigPath::root(), &mut paths);
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn validate_v0_25_typed_input(content: &str) -> RailResult<()> {
    let input: V0_25CompatibilityInput = toml_edit::de::from_str(content)
        .map_err(|error| RailError::message(format!("invalid v0.25.0 configuration: {error}")))?;

    let legacy_transitive = input.unify.pin_transitives.is_some() || input.unify.transitive_host.is_some();
    if input.unify.transitive_pinning.is_some() && legacy_transitive {
        return Err(RailError::message(
            "unify.transitive_pinning cannot be combined with v0.25.0 pin_transitives or transitive_host",
        ));
    }
    let legacy_msrv = input.unify.msrv.is_some()
        || input.unify.enforce_msrv_inheritance.is_some()
        || input.unify.msrv_source.is_some();
    if input.unify.msrv_policy.is_some() && legacy_msrv {
        return Err(RailError::message(
            "unify.msrv_policy cannot be combined with v0.25.0 msrv, msrv_source, or enforce_msrv_inheritance",
        ));
    }

    let legacy_remote =
        input.release.create_github_release.is_some() || input.release.forge.is_some() || input.release.push.is_some();
    if input.release.remote_effects.is_some() && legacy_remote {
        return Err(RailError::message(
            "release.remote_effects cannot be combined with v0.25.0 release.push, release.create_github_release, or release.forge",
        ));
    }
    if input.release.create_github_release == Some(true) && input.release.push != Some(true) {
        return Err(RailError::message(
            "v0.25.0 release.create_github_release = true requires release.push = true",
        ));
    }

    Ok(())
}

fn migrate_unify_policies(doc: &mut DocumentMut, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
    let Some(unify) = doc.get_mut("unify").and_then(Item::as_table_like_mut) else {
        return Ok(());
    };

    let legacy_transitive = ["pin_transitives", "transitive_host"]
        .into_iter()
        .filter(|key| unify.contains_key(key))
        .collect::<Vec<_>>();
    if !legacy_transitive.is_empty() {
        let replacement = if let Some(item) = unify.get("transitive_pinning") {
            format!("unify.transitive_pinning = {}", display_item(item))
        } else {
            let enabled = table_bool(unify, "pin_transitives", "unify")?.unwrap_or(false);
            let host = table_string(unify, "transitive_host", "unify")?.unwrap_or_else(|| "root".to_string());
            if enabled {
                let mut policy = InlineTable::new();
                policy.insert("host", host.into());
                let value = Value::InlineTable(policy);
                let rendered = value.to_string();
                unify.insert("transitive_pinning", Item::Value(value));
                format!("unify.transitive_pinning = {rendered}")
            } else {
                "field omitted (transitive pinning defaults to disabled)".to_string()
            }
        };
        for key in legacy_transitive {
            unify.remove(key);
            changes.push(MigrationChange {
                kind: "merge",
                path: format!("unify.{key}"),
                replacement: Some(replacement.clone()),
                message: "The legacy enable/host pair is now one typed pinning policy.",
            });
        }
    }

    let legacy_msrv = ["msrv", "msrv_source", "enforce_msrv_inheritance"]
        .into_iter()
        .filter(|key| unify.contains_key(key))
        .collect::<Vec<_>>();
    if !legacy_msrv.is_empty() {
        let replacement = if let Some(item) = unify.get("msrv_policy") {
            format!("unify.msrv_policy = {}", display_item(item))
        } else {
            let enabled = table_bool(unify, "msrv", "unify")?.unwrap_or(true);
            let inherit = table_bool(unify, "enforce_msrv_inheritance", "unify")?.unwrap_or(false);
            if !enabled && inherit {
                return Err(RailError::with_help(
                    "cannot migrate unify.enforce_msrv_inheritance = true with unify.msrv = false",
                    "enable MSRV computation or disable inheritance before migrating",
                ));
            }
            let source = table_string(unify, "msrv_source", "unify")?.unwrap_or_else(|| "max".to_string());
            if !matches!(source.as_str(), "deps" | "workspace" | "max") {
                return Err(RailError::message(format!(
                    "unify.msrv_source has unsupported value '{source}'"
                )));
            }
            if enabled && source == "max" && !inherit {
                "field omitted (MSRV compute/max defaults apply)".to_string()
            } else {
                let mut policy = InlineTable::new();
                policy.insert("mode", (if enabled { "compute" } else { "disabled" }).into());
                if enabled {
                    policy.insert("source", source.into());
                    if inherit {
                        policy.insert("inherit", true.into());
                    }
                }
                let value = Value::InlineTable(policy);
                let rendered = value.to_string();
                unify.insert("msrv_policy", Item::Value(value));
                format!("unify.msrv_policy = {rendered}")
            }
        };
        for key in legacy_msrv {
            unify.remove(key);
            changes.push(MigrationChange {
                kind: "merge",
                path: format!("unify.{key}"),
                replacement: Some(replacement.clone()),
                message: "The legacy MSRV fields are now one typed policy.",
            });
        }
    }

    Ok(())
}

fn migrate_release_remote_effects(doc: &mut DocumentMut, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
    let Some(release) = doc.get_mut("release").and_then(Item::as_table_like_mut) else {
        return Ok(());
    };
    let legacy = ["push", "create_github_release", "forge"]
        .into_iter()
        .filter(|key| release.contains_key(key))
        .collect::<Vec<_>>();
    if legacy.is_empty() {
        return Ok(());
    }

    let replacement = if let Some(item) = release.get("remote_effects") {
        format!("release.remote_effects = {}", display_item(item))
    } else {
        let push = table_bool(release, "push", "release")?.unwrap_or(false);
        let create_release = table_bool(release, "create_github_release", "release")?.unwrap_or(false);
        if create_release && !push {
            return Err(RailError::with_help(
                "cannot migrate release.create_github_release = true with release.push = false",
                "enable release.push or disable release.create_github_release before migrating",
            ));
        }
        let forge = table_string(release, "forge", "release")?.unwrap_or_else(|| "auto".to_string());
        let remote_effects = if create_release {
            match forge.as_str() {
                "auto" | "github" | "gitlab" => forge,
                value => {
                    return Err(RailError::message(format!(
                        "release.forge has unsupported value '{value}'"
                    )));
                }
            }
        } else if push {
            "push".to_string()
        } else {
            "none".to_string()
        };
        if remote_effects == "none" {
            "field omitted (remote effects default to \"none\")".to_string()
        } else {
            release.insert("remote_effects", Item::Value(remote_effects.as_str().into()));
            format!("release.remote_effects = \"{remote_effects}\"")
        }
    };
    for key in legacy {
        release.remove(key);
        changes.push(MigrationChange {
            kind: "merge",
            path: format!("release.{key}"),
            replacement: Some(replacement.clone()),
            message: "The legacy release-effect matrix is now one typed policy.",
        });
    }
    Ok(())
}

fn migrate_split_paths(
    doc: &mut DocumentMut,
    changes: &mut Vec<MigrationChange>,
    read_manifest: &mut impl FnMut(&Path) -> RailResult<Vec<u8>>,
) -> RailResult<()> {
    let crate_names = doc
        .get("crates")
        .and_then(Item::as_table_like)
        .map(|crates| crates.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    for crate_name in crate_names {
        let Some(split) = doc
            .get_mut("crates")
            .and_then(Item::as_table_like_mut)
            .and_then(|crates| crates.get_mut(&crate_name))
            .and_then(Item::as_table_like_mut)
            .and_then(|crate_config| crate_config.get_mut("split"))
            .and_then(Item::as_table_like_mut)
        else {
            continue;
        };
        let old_path = render_path(&["crates", &crate_name, "split", "paths"]);
        let new_path = render_path(&["crates", &crate_name, "split", "members"]);
        let relative_paths = if let Some(paths) = split.get("paths").and_then(Item::as_array) {
            paths
                .iter()
                .map(|entry| {
                    entry
                        .as_inline_table()
                        .and_then(|table| table.get("crate"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| {
                            RailError::message(format!("{old_path} entries must contain one string `crate` path"))
                        })
                })
                .collect::<RailResult<Vec<_>>>()?
        } else if let Some(paths) = split.get("paths").and_then(Item::as_array_of_tables) {
            paths
                .iter()
                .map(|entry| {
                    entry
                        .get("crate")
                        .and_then(Item::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| {
                            RailError::message(format!("{old_path} entries must contain one string `crate` path"))
                        })
                })
                .collect::<RailResult<Vec<_>>>()?
        } else {
            continue;
        };
        let mut members = Vec::with_capacity(relative_paths.len());
        for relative in relative_paths {
            let relative = Path::new(&relative);
            if relative.is_absolute() {
                return Err(RailError::message(format!(
                    "cannot migrate split member path '{}': path must stay inside the workspace",
                    relative.display()
                )));
            }
            let mut normalized_relative = std::path::PathBuf::new();
            for component in relative.components() {
                match component {
                    Component::CurDir => {}
                    Component::Normal(component) => normalized_relative.push(component),
                    Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                        return Err(RailError::message(format!(
                            "cannot migrate split member path '{}': path must stay inside the workspace",
                            relative.display()
                        )));
                    }
                }
            }
            if normalized_relative.as_os_str().is_empty() {
                normalized_relative.push(".");
            }
            let manifest = read_manifest(&normalized_relative)?;
            let manifest = std::str::from_utf8(&manifest).map_err(|error| {
                RailError::message(format!(
                    "cannot migrate split member path '{}': Cargo.toml is not UTF-8: {error}",
                    relative.display()
                ))
            })?;
            let manifest: DocumentMut = manifest.parse().map_err(|error: toml_edit::TomlError| {
                RailError::message(format!(
                    "cannot migrate split member path '{}': invalid Cargo.toml: {error}",
                    relative.display()
                ))
            })?;
            let package_name = manifest
                .get("package")
                .and_then(Item::as_table_like)
                .and_then(|package| package.get("name"))
                .and_then(Item::as_str)
                .ok_or_else(|| {
                    RailError::message(format!(
                        "cannot migrate split member path '{}': Cargo.toml has no package.name",
                        relative.display()
                    ))
                })?;
            members.push(package_name.to_string());
        }
        members.sort();
        members.dedup();

        if let Some(existing) = split.get("members").and_then(Item::as_array) {
            let mut configured = existing
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| RailError::message(format!("{new_path} must contain only Cargo member names")))
                })
                .collect::<RailResult<Vec<_>>>()?;
            configured.sort();
            configured.dedup();
            if configured != members {
                return Err(RailError::message(format!(
                    "cannot migrate {old_path}: existing {new_path} selects different Cargo members"
                )));
            }
        } else {
            let mut array = Array::new();
            for member in &members {
                array.push(member.as_str());
            }
            split.insert("members", Item::Value(Value::Array(array)));
        }
        split.remove("paths");
        changes.push(MigrationChange {
            kind: "replace",
            path: old_path,
            replacement: Some(new_path),
            message: "Split ownership is now resolved from Cargo member names.",
        });
    }
    Ok(())
}

fn remove_changelog_configuration(doc: &mut DocumentMut, changes: &mut Vec<MigrationChange>) {
    for (key, message) in CHANGELOG_REMOVALS {
        remove_fixed(doc, changes, &format!("release.changelog.{key}"), message);
    }

    let crate_names = doc
        .get("crates")
        .and_then(Item::as_table_like)
        .map(|crates| crates.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    for crate_name in crate_names {
        for (key, message) in CHANGELOG_REMOVALS {
            let segments = ["crates", crate_name.as_str(), "changelog", *key];
            if remove_segments(doc.as_table_mut(), &segments) {
                changes.push(MigrationChange {
                    kind: "remove",
                    path: render_path(&segments),
                    replacement: None,
                    message,
                });
            }
        }
    }
}

fn remove_reserved_sync_tables(doc: &mut DocumentMut, changes: &mut Vec<MigrationChange>) {
    let crate_names = doc
        .get("crates")
        .and_then(Item::as_table_like)
        .map(|crates| crates.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    for crate_name in crate_names {
        let segments = ["crates", crate_name.as_str(), "sync"];
        if remove_segments(doc.as_table_mut(), &segments) {
            changes.push(MigrationChange {
                kind: "remove",
                path: render_path(&segments),
                replacement: None,
                message: "The reserved per-crate sync table had no behavior.",
            });
        }
    }
}

fn remove_fixed(doc: &mut DocumentMut, changes: &mut Vec<MigrationChange>, path: &str, message: &'static str) {
    let segments = path.split('.').collect::<Vec<_>>();
    if remove_segments(doc.as_table_mut(), &segments) {
        changes.push(MigrationChange {
            kind: "remove",
            path: path.to_string(),
            replacement: None,
            message,
        });
    }
}

fn remove_segments(table: &mut dyn TableLike, segments: &[&str]) -> bool {
    let Some((segment, remaining)) = segments.split_first() else {
        return false;
    };
    if remaining.is_empty() {
        return table.remove(segment).is_some();
    }
    table
        .get_mut(segment)
        .and_then(Item::as_table_like_mut)
        .is_some_and(|child| remove_segments(child, remaining))
}

fn table_bool(table: &dyn TableLike, key: &str, prefix: &str) -> RailResult<Option<bool>> {
    table
        .get(key)
        .map(|item| {
            item.as_bool()
                .ok_or_else(|| RailError::message(format!("{prefix}.{key} must be a boolean before migration")))
        })
        .transpose()
}

fn table_string(table: &dyn TableLike, key: &str, prefix: &str) -> RailResult<Option<String>> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| RailError::message(format!("{prefix}.{key} must be a string before migration")))
        })
        .transpose()
}

fn display_item(item: &Item) -> String {
    item.as_value()
        .map(ToString::to_string)
        .unwrap_or_else(|| item.to_string())
}

fn render_path(segments: &[&str]) -> String {
    let mut path = schema::ConfigPath::root();
    for segment in segments {
        path = path.child(*segment);
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    const TAGGED_CONFIG: &[u8] = include_bytes!("../../tests/fixtures/config/v0.25.0/rail.toml");

    #[test]
    fn exact_tagged_configuration_normalizes_to_current_facts() {
        let digest = Sha256::digest(TAGGED_CONFIG)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest,
            "eea3d2e8e080e2542c88f44032405394eb1abb577c07d38a7d1cb13519f585bc"
        );

        let normalized = normalize(TAGGED_CONFIG, |_| {
            Err(RailError::message("fixture unexpectedly requested a split manifest"))
        })
        .unwrap();
        let migrated = String::from_utf8(normalized.bytes).unwrap();
        assert!(normalized.config.plan.work.contains_key("compatibility"));
        assert_eq!(normalized.config.release.tag_format, "{prefix}{version}");
        assert!(migrated.contains("# Repository policy. Omitted fields use cargo-rail's coded defaults."));
        assert!(migrated.contains("[plan.work.compatibility]"));
        assert!(!migrated.contains("require_change_files"));
        assert!(!migrated.contains("[release.changelog.filters]"));
        assert!(
            normalized
                .changes
                .iter()
                .any(|change| change.path == "release.require_change_files")
        );
    }

    #[test]
    fn predecessor_shape_does_not_admit_unknown_keys() {
        let bytes = b"[release]\nrequire_change_files = true\nrequire_change_fiels = true\n";
        let error = normalize(bytes, |_| {
            Err(RailError::message("fixture unexpectedly requested a split manifest"))
        })
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("unknown configuration key 'release.require_change_fiels'")
        );
    }

    #[test]
    fn current_configuration_is_byte_identical_and_has_no_migrations() {
        let bytes = b"# keep this byte-for-byte\n[release]\nsemver_check = \"deny\"\n";
        let normalized = normalize(bytes, |_| {
            Err(RailError::message("fixture unexpectedly requested a split manifest"))
        })
        .unwrap();
        assert_eq!(normalized.bytes, bytes);
        assert!(normalized.changes.is_empty());
    }
}
