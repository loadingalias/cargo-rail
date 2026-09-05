//! Supported predecessor spellings, translated in memory without configuration writes.

use super::schema;
use crate::error::{RailError, RailResult};
use std::path::{Component, Path};
use toml_edit::{Array, DocumentMut, InlineTable, Item, TableLike, Value};

/// Optional provenance for one accepted predecessor spelling.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Compatibility {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) replacement: Option<String>,
    pub(crate) message: &'static str,
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
    #[serde(rename = "require_clean")]
    _require_clean: Option<bool>,
    #[serde(rename = "publish_delay")]
    _publish_delay: Option<u64>,
    remote_effects: Option<serde::de::IgnoredAny>,
    create_github_release: Option<bool>,
    forge: Option<V0_25ReleaseForge>,
    push: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum V0_25ReleaseForge {
    Auto,
    Github,
    Gitlab,
}

#[derive(serde::Deserialize, Default)]
struct V0_25CrateInput {
    #[serde(rename = "split")]
    _split: Option<V0_25SplitInput>,
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
    "release.require_clean",
    "release.publish_delay",
    "release.create_github_release",
    "release.forge",
    "release.push",
    "crates.<name>.split.paths",
    "crates.<name>.sync",
];

const REMOVED_FIELDS: &[(&str, &str)] = &[
    ("unify.compiler_diag_cache", "Compiler evidence caching is automatic."),
    ("unify.sort_dependencies", "Dependency edits are always deterministic."),
    (
        "unify.prune_dead_features",
        "Dead-feature diagnostics are unconditional; deletion still requires closed-consumer proof.",
    ),
    (
        "unify.detect_unused",
        "Unused-dependency diagnostics are unconditional.",
    ),
    (
        "unify.remove_unused",
        "Read-only checks and explicit apply now define the mutation boundary.",
    ),
    (
        "unify.detect_undeclared_features",
        "Borrowed-feature diagnostics are unconditional.",
    ),
    (
        "unify.fix_undeclared_features",
        "Read-only checks and explicit apply now define the mutation boundary.",
    ),
    (
        "release.require_clean",
        "Release cleanliness is enforced by fixed preview/apply semantics.",
    ),
    (
        "release.publish_delay",
        "Registry convergence is an explicit stop-and-resume boundary.",
    ),
    ("workspace", "The reserved workspace table had no behavior."),
    ("toolchain", "The reserved toolchain table had no behavior."),
];

/// Translate only recognized predecessor input. Current input does not request workspace facts.
pub(super) fn normalize_document(
    doc: &mut DocumentMut,
    mut resolve_member: impl FnMut(&Path) -> RailResult<String>,
) -> RailResult<Vec<Compatibility>> {
    let paths = schema::document_paths(doc);
    if let Some(path) = paths
        .iter()
        .find(|path| !schema::is_known_config_path(path) && !is_retired_v0_25_path(path))
    {
        return Err(RailError::message(format!("unknown configuration key '{path}'")));
    }
    if !paths
        .iter()
        .any(|path| !schema::is_known_config_path(path) && is_retired_v0_25_path(path))
    {
        return Ok(Vec::new());
    }
    validate_v0_25_typed_input(doc)?;
    let mut changes = Vec::new();
    decode_unify_policies(doc, &mut changes)?;
    decode_release_remote_effects(doc, &mut changes)?;
    decode_split_paths(doc, &mut changes, &mut resolve_member)?;
    for (path, message) in REMOVED_FIELDS {
        remove_fixed(doc, &mut changes, path, message);
    }
    remove_reserved_sync_tables(doc, &mut changes);
    Ok(changes)
}

fn is_retired_v0_25_path(path: &schema::ConfigPath) -> bool {
    RETIRED_V0_25_FIELDS
        .iter()
        .any(|pattern| paths_share_prefix(pattern, path.segments()))
}

fn paths_share_prefix(pattern: &str, actual: &[String]) -> bool {
    actual
        .iter()
        .zip(pattern.split('.'))
        .all(|(actual, expected)| expected.starts_with('<') || actual == expected)
}

fn validate_v0_25_typed_input(doc: &DocumentMut) -> RailResult<()> {
    let input: V0_25CompatibilityInput = toml_edit::de::from_document(doc.clone())
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

fn decode_unify_policies(doc: &mut DocumentMut, changes: &mut Vec<Compatibility>) -> RailResult<()> {
    let Some(unify) = doc.get_mut("unify").and_then(Item::as_table_like_mut) else {
        return Ok(());
    };

    let legacy_transitive = ["pin_transitives", "transitive_host"]
        .into_iter()
        .filter(|key| unify.contains_key(key))
        .collect::<Vec<_>>();
    if !legacy_transitive.is_empty() {
        let replacement = {
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
            changes.push(Compatibility {
                kind: "merge",
                path: format!("unify.{key}"),
                replacement: Some(replacement.clone()),
                message: "The legacy enable/host pair forms one typed pinning policy.",
            });
        }
    }

    let legacy_msrv = ["msrv", "msrv_source", "enforce_msrv_inheritance"]
        .into_iter()
        .filter(|key| unify.contains_key(key))
        .collect::<Vec<_>>();
    if !legacy_msrv.is_empty() {
        let replacement = {
            let enabled = table_bool(unify, "msrv", "unify")?.unwrap_or(true);
            let inherit = table_bool(unify, "enforce_msrv_inheritance", "unify")?.unwrap_or(false);
            if !enabled && inherit {
                return Err(RailError::with_help(
                    "cannot interpret unify.enforce_msrv_inheritance = true with unify.msrv = false",
                    "enable MSRV computation or disable inheritance",
                ));
            }
            let source = table_string(unify, "msrv_source", "unify")?.unwrap_or_else(|| "max".to_string());
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
        };
        for key in legacy_msrv {
            unify.remove(key);
            changes.push(Compatibility {
                kind: "merge",
                path: format!("unify.{key}"),
                replacement: Some(replacement.clone()),
                message: "The legacy MSRV fields form one typed policy.",
            });
        }
    }

    Ok(())
}

fn decode_release_remote_effects(doc: &mut DocumentMut, changes: &mut Vec<Compatibility>) -> RailResult<()> {
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

    let replacement = {
        let push = table_bool(release, "push", "release")?.unwrap_or(false);
        let create_release = table_bool(release, "create_github_release", "release")?.unwrap_or(false);
        let forge = table_string(release, "forge", "release")?.unwrap_or_else(|| "auto".to_string());
        let remote_effects = if create_release {
            forge
        } else if push {
            "push".to_string()
        } else {
            "none".to_string()
        };
        release.insert("remote_effects", Item::Value(remote_effects.as_str().into()));
        format!("release.remote_effects = \"{remote_effects}\"")
    };
    for key in legacy {
        release.remove(key);
        changes.push(Compatibility {
            kind: "merge",
            path: format!("release.{key}"),
            replacement: Some(replacement.clone()),
            message: "The legacy release-effect matrix forms one typed policy.",
        });
    }
    Ok(())
}

fn decode_split_paths(
    doc: &mut DocumentMut,
    changes: &mut Vec<Compatibility>,
    resolve_member: &mut impl FnMut(&Path) -> RailResult<String>,
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
                    "cannot interpret split member path '{}': path must stay inside the workspace",
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
                            "cannot interpret split member path '{}': path must stay inside the workspace",
                            relative.display()
                        )));
                    }
                }
            }
            if normalized_relative.as_os_str().is_empty() {
                normalized_relative.push(".");
            }
            members.push(resolve_member(&normalized_relative)?);
        }
        members.sort();
        members.dedup();

        if let Some(existing) = split.get("members") {
            let existing = existing
                .as_array()
                .ok_or_else(|| RailError::message(format!("{new_path} must be an array of Cargo member names")))?;
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
                    "cannot interpret {old_path}: existing {new_path} selects different Cargo members"
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
        changes.push(Compatibility {
            kind: "replace",
            path: old_path,
            replacement: Some(new_path),
            message: "Split ownership is resolved from Cargo member names.",
        });
    }
    Ok(())
}

fn remove_reserved_sync_tables(doc: &mut DocumentMut, changes: &mut Vec<Compatibility>) {
    let crate_names = doc
        .get("crates")
        .and_then(Item::as_table_like)
        .map(|crates| crates.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    for crate_name in crate_names {
        let segments = ["crates", crate_name.as_str(), "sync"];
        if remove_segments(doc.as_table_mut(), &segments) {
            changes.push(Compatibility {
                kind: "inert",
                path: render_path(&segments),
                replacement: None,
                message: "The reserved per-crate sync table had no behavior.",
            });
        }
    }
}

fn remove_fixed(doc: &mut DocumentMut, changes: &mut Vec<Compatibility>, path: &str, message: &'static str) {
    let segments = path.split('.').collect::<Vec<_>>();
    if remove_segments(doc.as_table_mut(), &segments) {
        changes.push(Compatibility {
            kind: "inert",
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
                .ok_or_else(|| RailError::message(format!("{prefix}.{key} must be a boolean")))
        })
        .transpose()
}

fn table_string(table: &dyn TableLike, key: &str, prefix: &str) -> RailResult<Option<String>> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| RailError::message(format!("{prefix}.{key} must be a string")))
        })
        .transpose()
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
    use crate::config::{RailConfig, ReleaseRemoteEffects, ReleaseSource, decode};
    use crate::error::RailError;

    fn policy(input: &str) -> RailConfig {
        decode(input.as_bytes(), |_| {
            Err(RailError::message("unexpected workspace lookup"))
        })
        .unwrap()
        .config
    }

    #[test]
    fn current_and_predecessor_unify_policies_agree() {
        for (old, current) in [
            ("pin_transitives = false\ntransitive_host = 'unused'", ""),
            ("transitive_host = 'unused'", ""),
            ("pin_transitives = true", "transitive_pinning = { host = 'root' }"),
            (
                "pin_transitives = true\ntransitive_host = 'crates/host'",
                "transitive_pinning = { host = 'crates/host' }",
            ),
            (
                "msrv = false\nmsrv_source = 'deps'",
                "msrv_policy = { mode = 'disabled' }",
            ),
            ("msrv = true", ""),
            (
                "msrv_source = 'workspace'",
                "msrv_policy = { mode = 'compute', source = 'workspace' }",
            ),
            (
                "msrv_source = 'deps'\nenforce_msrv_inheritance = true",
                "msrv_policy = { mode = 'compute', source = 'deps', inherit = true }",
            ),
            (
                "msrv_source = 'max'\nenforce_msrv_inheritance = true",
                "msrv_policy = { mode = 'compute', source = 'max', inherit = true }",
            ),
        ] {
            assert_eq!(
                serde_json::to_value(policy(&format!("[unify]\n{old}"))).unwrap(),
                serde_json::to_value(policy(&format!("[unify]\n{current}"))).unwrap(),
                "{old}"
            );
        }
    }

    #[test]
    fn predecessor_remote_effects_preserve_authority_ceiling() {
        for push in [false, true] {
            for create in [false, true] {
                for (forge, effect) in [
                    ("auto", ReleaseRemoteEffects::Auto),
                    ("github", ReleaseRemoteEffects::Github),
                    ("gitlab", ReleaseRemoteEffects::Gitlab),
                ] {
                    let input =
                        format!("[release]\npush = {push}\ncreate_github_release = {create}\nforge = '{forge}'");
                    let decoded =
                        crate::config::decode_without_workspace(input.as_bytes()).map(|decoded| decoded.config);
                    if create && !push {
                        assert!(
                            decoded
                                .unwrap_err()
                                .to_string()
                                .contains("requires release.push = true")
                        );
                    } else {
                        let config = decoded.unwrap();
                        assert_eq!(
                            config.release.remote_effects,
                            if create {
                                effect
                            } else if push {
                                ReleaseRemoteEffects::Push
                            } else {
                                ReleaseRemoteEffects::None
                            }
                        );
                        assert_eq!(config.release.registry_publication, Default::default());
                    }
                }
            }
        }
        assert_eq!(
            policy("[release]\nforge = 'gitlab'").release.remote_effects,
            ReleaseRemoteEffects::None
        );
    }

    #[test]
    fn inert_predecessor_fields_do_not_change_policy_or_hide_unknown_keys() {
        let inert = "workspace = { anything = { nested = 7 } }\ntoolchain = false\n[unify]\nprune_dead_features = false\ndetect_unused = false\ncompiler_diag_cache = false\nremove_unused = false\ndetect_undeclared_features = false\nfix_undeclared_features = false\nsort_dependencies = false\n[release]\nrequire_clean = false\npublish_delay = 999\n";
        assert_eq!(
            serde_json::to_value(policy(inert)).unwrap(),
            serde_json::to_value(RailConfig::default()).unwrap()
        );
        let config = policy("[crates.demo.sync]\nunknown = { nested = true }");
        assert!(config.crates["demo"].split.is_none());
        for invalid in [
            "[release]\npush = false\nforg = 'github'",
            "[unify]\nmsrv = false\nenforce_msrv_inheritance = true",
            "[unify]\nmsrv = false\nmsrv_source = 'invalid'",
            "[unify]\nmsrv = false\nmsrv_policy = { mode = 'disabled' }",
            "[unify]\npin_transitives = false\ntransitive_pinning = { host = 'root' }",
            "[unify]\npin_transitives = false\ntransitive_host = 5",
            "[release]\nremote_effects = 'none'\npush = false",
            "[release]\nrequire_clean = 'false'",
            "[release]\npublish_delay = -1",
            "[run]\ncommand = 'ignored?'",
        ] {
            assert!(
                crate::config::decode_without_workspace(invalid.as_bytes()).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn release_policy_survives_combined_current_and_predecessor_input() {
        let config = policy(
            "[release]\nsource = 'commits'\npush = true\nrequire_change_files = ['core']\nrequire_release_notes = false\nrequire_changelog_entries = true\nunconventional_commits = 'deny'\nrelease_notes_dir = 'notes'\n[release.changelog]\nemoji = false\nentry_format = '- {description}'\ngroup_order = ['fix', 'feat']\nfallback = 'skip'\n[release.changelog.filters]\nskip_types = ['docs']\n[crates.core.changelog]\npath = 'HISTORY.md'\n",
        );
        assert_eq!(config.release.source, ReleaseSource::Commits);
        assert!(!config.release.require_release_notes);
        assert!(config.release.require_changelog_entries);
        assert!(config.release.requires_change_file("core"));
        assert!(!config.release.requires_change_file("other"));
        assert_eq!(config.release.changelog.entry_format, "- {description}");
        assert_eq!(config.release.changelog.filters.skip_types, ["docs"]);
        assert_eq!(config.release.release_notes_dir, "notes");
    }

    #[test]
    fn current_and_tagged_input_need_no_compatibility_workspace_lookup() {
        for bytes in [
            b"# unchanged\n[release]\nsemver_check = 'deny'\n".as_slice(),
            include_bytes!("../../tests/fixtures/config/v0.25.0/rail.toml").as_slice(),
        ] {
            let decoded = decode(bytes, |_| Err(RailError::message("unexpected workspace lookup"))).unwrap();
            assert!(decoded.compatibility.is_empty());
            decoded.config.validate_policy().unwrap();
        }
    }
}
