//! Sparse configuration migration.
//!
//! A setting is removed only when removing it leaves the decoded effective policy unchanged,
//! so the migration compares policy, not TOML text. That prunes explicit current defaults and
//! legacy spellings of a default, and keeps comments, ordering, and every other setting.
//! Keys that an earlier release removed have no current meaning and are removed first;
//! the preview names the release that retired each one.

use serde::Serialize;
use toml_edit::{DocumentMut, Item, Table};

use crate::config::{RailConfig, decode};
use crate::error::{RailError, RailResult};

/// One setting the migration removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RemovedSetting {
    /// Dotted configuration path; array-of-tables entries use `[index]`.
    pub(crate) path: String,
    /// The removed TOML value, or `table` for an emptied table header.
    pub(crate) value: String,
    /// The release that retired this key, when the key no longer exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) removed_in: Option<&'static str>,
}

/// One setting the migration writes in place of keys an earlier release retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AddedSetting {
    /// Dotted configuration path.
    pub(crate) path: String,
    /// The written TOML value.
    pub(crate) value: String,
}

/// The file content after migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MigratedFile {
    /// Nothing to remove.
    Unchanged,
    /// Write this sparse content.
    Rewrite(String),
    /// Every setting restated a default; delete the file.
    Delete,
}

/// A previewable sparse migration of one configuration source.
#[derive(Debug, Clone)]
pub(crate) struct ConfigMigration {
    pub(crate) removed: Vec<RemovedSetting>,
    /// Replacements written for retired keys; they keep the retired keys' effect.
    pub(crate) added: Vec<AddedSetting>,
    /// Comment lines that `config print` generated and that no longer describe the file.
    pub(crate) removed_comments: Vec<String>,
    pub(crate) file: MigratedFile,
}

/// The first line `config print` writes; the second follows it exactly.
const PRINT_HEADER_PREFIX: &str = "# Effective configuration (";
const PRINT_HEADER_NOTE: &str = "# This shows all settings including defaults for unset fields.";

fn is_print_header(line: &str) -> bool {
    let line = line.trim_end();
    line == PRINT_HEADER_NOTE || (line.starts_with(PRINT_HEADER_PREFIX) && line.ends_with(')'))
}

/// Remove `config print`'s generated header lines, and the blank line each block leaves behind.
fn without_print_header(content: &str) -> String {
    let mut kept = Vec::new();
    let mut skipped = false;
    for line in content.split_inclusive('\n') {
        if is_print_header(line) {
            skipped = true;
            continue;
        }
        if skipped && line.trim().is_empty() {
            skipped = false;
            continue;
        }
        skipped = false;
        kept.push(line);
    }
    kept.concat()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Segment {
    Key(String),
    Index(usize),
}

/// Plan the sparse form of `bytes`.
///
/// `deletable` states whether deleting the file leaves the default policy in effect; it is false
/// when the file was selected explicitly or shadows another discovery candidate.
pub(crate) fn plan(bytes: &[u8], deletable: bool) -> RailResult<ConfigMigration> {
    let content = std::str::from_utf8(bytes)
        .map_err(|error| RailError::message(format!("configuration is not valid UTF-8: {error}")))?;
    let mut document: DocumentMut = content
        .parse()
        .map_err(|error: toml_edit::TomlError| RailError::message(error.to_string()))?;
    // Retired release keys first become their replacements, so removing them keeps their effect.
    let added = rewrite_retired_release(&mut document);
    // Retired keys have no current meaning, so removing them cannot change policy.
    let mut removed = strip_retired(&mut document);
    let original = decode(document.to_string().as_bytes())?;
    let policy = effective(&original.config)?;
    let mut document = original.document;

    for path in leaf_paths(document.as_table()) {
        try_remove(&mut document, &path, &policy, &mut removed)?;
    }
    // Deepest tables first, so a parent emptied by its children is considered after them.
    let mut tables = table_paths(document.as_table());
    tables.sort_by_key(|path| std::cmp::Reverse(path.len()));
    for path in tables {
        if item_at(&document, &path)
            .and_then(Item::as_table)
            .is_some_and(Table::is_empty)
        {
            try_remove(&mut document, &path, &policy, &mut removed)?;
        }
    }

    let source = String::from_utf8_lossy(bytes);
    let removed_comments = source
        .lines()
        .filter(|line| is_print_header(line))
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();
    let file = if removed.is_empty() && added.is_empty() && removed_comments.is_empty() {
        MigratedFile::Unchanged
    } else if document.as_table().is_empty() && deletable {
        MigratedFile::Delete
    } else {
        let source = without_print_header(&source);
        let content = without_print_header(&document.to_string());
        MigratedFile::Rewrite(with_file_header(source.as_bytes(), &content))
    };
    let migrated_policy = match &file {
        MigratedFile::Unchanged => policy.clone(),
        MigratedFile::Delete => effective(&RailConfig::default())?,
        MigratedFile::Rewrite(content) => effective(&decode(content.as_bytes())?.config)?,
    };
    if migrated_policy != policy {
        return Err(RailError::message(
            "configuration migration would change effective policy; no migration was planned",
        ));
    }
    Ok(ConfigMigration {
        removed,
        added,
        removed_comments,
        file,
    })
}

/// The `[release]` keys Cargo-Rail 0.32.0 retired when releases stopped keeping records.
const RETIRED_RELEASE_KEYS: &[&str] = &[
    "remote_effects",
    "registry_publication",
    "hosted_workflow",
    "validation",
    "artifacts",
    "tag_prefix",
    "require_change_files",
    "release_notes_dir",
    "require_release_notes",
];

/// Write the replacements for retired `[release]` keys, keeping what they did.
///
/// It runs only while a retired key or a `{prefix}` tag placeholder remains, so a file that was
/// already migrated, or written for the current release, is never rewritten.
pub(crate) fn rewrite_retired_release(document: &mut DocumentMut) -> Vec<AddedSetting> {
    let placeholder = format!("{}prefix{}", '{', '}');
    let crate_formats = document
        .get("crates")
        .and_then(Item::as_table)
        .map(|crates| {
            crates
                .iter()
                .filter_map(|(name, item)| {
                    let format = item.get("release")?.get("tag_format")?.as_str()?;
                    format
                        .contains(&placeholder)
                        .then(|| (name.to_owned(), format.to_owned()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(release) = document.get_mut("release").and_then(Item::as_table_mut) else {
        return Vec::new();
    };
    let text = |release: &Table, key: &str| release.get(key).and_then(Item::as_str).map(str::to_owned);
    let format = text(release, "tag_format");
    // Retired keys or the retired `{prefix}` placeholder identify a file written before 0.32.0,
    // whose registry and GitHub effects were off by default.
    let retired = RETIRED_RELEASE_KEYS.iter().any(|key| release.contains_key(key))
        || format.as_ref().is_some_and(|format| format.contains(&placeholder))
        || !crate_formats.is_empty();
    if !retired {
        return Vec::new();
    }
    let mut added = Vec::new();
    let mut set = |release: &mut Table, key: &str, value: toml_edit::Value| {
        added.push(AddedSetting {
            path: format!("release.{key}"),
            value: value.to_string().trim().to_owned(),
        });
        release.insert(key, Item::Value(value));
    };
    let prefix = text(release, "tag_prefix").unwrap_or_else(|| "v".to_owned());
    match &format {
        Some(format) if format.contains(&placeholder) => {
            set(release, "tag_format", format.replace(&placeholder, &prefix).into());
        }
        // The retired default was "{crate}-{prefix}{version}".
        None if prefix != "v" => set(release, "tag_format", format!("{{crate}}-{prefix}{{version}}").into()),
        _ => {}
    }
    if retired {
        if text(release, "registry_publication").as_deref() != Some("crates-io") && !release.contains_key("publish") {
            set(release, "publish", "none".into());
        }
        if !matches!(text(release, "remote_effects").as_deref(), Some("github" | "auto")) {
            if !release.contains_key("github_release") {
                set(release, "github_release", false.into());
            }
            if !release.contains_key("checks") {
                set(release, "checks", "none".into());
            }
        }
        let inventories = release
            .get("artifacts")
            .and_then(Item::as_table_like)
            .map(|artifacts| {
                artifacts
                    .iter()
                    .filter_map(|(name, entry)| {
                        let files = entry.get("files")?.as_table_like()?;
                        Some((
                            name.to_owned(),
                            files.iter().map(|(file, _)| file.to_owned()).collect::<Vec<_>>(),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !inventories.is_empty() && !release.contains_key("assets") {
            let mut assets = Table::new();
            for (name, files) in inventories {
                let value = toml_edit::Value::Array(files.iter().map(String::as_str).collect());
                added.push(AddedSetting {
                    path: format!("release.assets.{name}"),
                    value: value.to_string().trim().to_owned(),
                });
                assets.insert(&name, Item::Value(value));
            }
            release.insert("assets", Item::Table(assets));
        }
    }
    if let Some(crates) = document.get_mut("crates").and_then(Item::as_table_mut) {
        for (name, format) in crate_formats {
            let value: toml_edit::Value = format.replace(&placeholder, &prefix).into();
            added.push(AddedSetting {
                path: format!("crates.{name}.release.tag_format"),
                value: value.to_string().trim().to_owned(),
            });
            if let Some(release) = crates
                .get_mut(&name)
                .and_then(|item| item.get_mut("release"))
                .and_then(Item::as_table_like_mut)
            {
                release.insert("tag_format", Item::Value(value));
            }
        }
    }
    added
}

/// Delete every removable retired key and report each one.
pub(crate) fn strip_retired(document: &mut DocumentMut) -> Vec<RemovedSetting> {
    let mut removed = Vec::new();
    for path in crate::config::schema::document_paths(document) {
        let Some((retired, key)) = crate::config::schema::retired_key(&path) else {
            continue;
        };
        if !retired.removable || key != path {
            continue;
        }
        let segments = path
            .segments()
            .iter()
            .map(|segment| Segment::Key(segment.clone()))
            .collect::<Vec<_>>();
        let value = item_at(document, &segments).map(|item| {
            if item.is_table_like() {
                "table".to_string()
            } else {
                item.to_string().trim().to_string()
            }
        });
        if let Some(value) = value
            && remove_at(document, &segments)
        {
            removed.push(RemovedSetting {
                path: path.to_string(),
                value,
                removed_in: Some(retired.removed_in),
            });
        }
    }
    removed
}

/// Keep the source's leading comment block, which toml_edit attaches to the first setting.
fn with_file_header(source: &[u8], content: &str) -> String {
    let source = String::from_utf8_lossy(source);
    let header = source
        .lines()
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        .collect::<Vec<_>>();
    let comments = header
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(&[][..], |last| &header[..=last]);
    let body = content.trim_start();
    if comments.is_empty() || body.starts_with(comments[0].trim_start()) {
        return body.to_string();
    }
    let separator = if body.is_empty() { "" } else { "\n" };
    format!("{}\n{separator}{body}", comments.join("\n"))
}

fn effective(config: &RailConfig) -> RailResult<serde_json::Value> {
    Ok(serde_json::to_value(config)?)
}

/// Remove `path` when the remaining document still decodes to the same effective policy.
fn try_remove(
    document: &mut DocumentMut,
    path: &[Segment],
    policy: &serde_json::Value,
    removed: &mut Vec<RemovedSetting>,
) -> RailResult<()> {
    let Some(item) = item_at(document, path) else {
        return Ok(());
    };
    let value = if item.is_table() {
        "table".to_string()
    } else {
        item.to_string().trim().to_string()
    };
    let mut candidate = document.clone();
    if !remove_at(&mut candidate, path) {
        return Ok(());
    }
    let Ok(decoded) = decode(candidate.to_string().as_bytes()) else {
        return Ok(());
    };
    if &effective(&decoded.config)? == policy {
        *document = candidate;
        removed.push(RemovedSetting {
            path: render(path),
            value,
            removed_in: None,
        });
    }
    Ok(())
}

fn leaf_paths(table: &Table) -> Vec<Vec<Segment>> {
    let mut paths = Vec::new();
    collect(table, &mut Vec::new(), &mut paths, false);
    paths
}

fn table_paths(table: &Table) -> Vec<Vec<Segment>> {
    let mut paths = Vec::new();
    collect(table, &mut Vec::new(), &mut paths, true);
    paths
}

fn collect(table: &Table, prefix: &mut Vec<Segment>, paths: &mut Vec<Vec<Segment>>, tables: bool) {
    for (key, item) in table.iter() {
        prefix.push(Segment::Key(key.to_string()));
        match item {
            Item::Table(child) => {
                if tables {
                    paths.push(prefix.clone());
                }
                collect(child, prefix, paths, tables);
            }
            Item::ArrayOfTables(entries) => {
                for (index, entry) in entries.iter().enumerate() {
                    prefix.push(Segment::Index(index));
                    collect(entry, prefix, paths, tables);
                    prefix.pop();
                }
            }
            Item::Value(_) if !tables => paths.push(prefix.clone()),
            Item::Value(_) | Item::None => {}
        }
        prefix.pop();
    }
}

fn item_at<'a>(document: &'a DocumentMut, path: &[Segment]) -> Option<&'a Item> {
    let mut table = document.as_table();
    let mut segments = path.iter().peekable();
    while let Some(segment) = segments.next() {
        let Segment::Key(key) = segment else {
            return None;
        };
        let item = table.get(key)?;
        match segments.peek() {
            None => return Some(item),
            Some(Segment::Index(index)) => {
                table = item.as_array_of_tables()?.get(*index)?;
                segments.next();
            }
            Some(Segment::Key(_)) => table = item.as_table()?,
        }
    }
    None
}

fn remove_at(document: &mut DocumentMut, path: &[Segment]) -> bool {
    let Some((Segment::Key(last), parents)) = path.split_last().map(|(last, parents)| (last.clone(), parents)) else {
        return false;
    };
    let mut table = document.as_table_mut();
    let mut segments = parents.iter();
    while let Some(segment) = segments.next() {
        match segment {
            Segment::Key(key) => match table.get_mut(key) {
                Some(Item::Table(child)) => table = child,
                Some(Item::ArrayOfTables(entries)) => {
                    let Some(Segment::Index(index)) = segments.next() else {
                        return false;
                    };
                    let Some(entry) = entries.get_mut(*index) else {
                        return false;
                    };
                    table = entry;
                }
                _ => return false,
            },
            Segment::Index(_) => return false,
        }
    }
    table.remove(&last).is_some()
}

fn render(path: &[Segment]) -> String {
    let mut rendered = String::new();
    for segment in path {
        match segment {
            Segment::Key(key) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push_str(key);
            }
            Segment::Index(index) => rendered.push_str(&format!("[{index}]")),
        }
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removed_paths(migration: &ConfigMigration) -> Vec<&str> {
        migration.removed.iter().map(|setting| setting.path.as_str()).collect()
    }

    #[test]
    fn explicit_defaults_and_legacy_spellings_are_pruned_while_policy_and_comments_stay() {
        let source = "# Project policy.\n\n[unify]\n# Keep one version per dependency.\nstrict_version_compat = true\ncompiler_targets = []\n\n# Why this matters.\ninclude_renamed = true\n";
        let migration = plan(source.as_bytes(), true).expect("migration");
        let MigratedFile::Rewrite(content) = &migration.file else {
            panic!("expected a rewrite: {:?}", migration.file);
        };
        assert!(
            removed_paths(&migration).contains(&"unify.compiler_targets"),
            "{migration:?}"
        );
        assert!(
            content.contains("# Why this matters.\ninclude_renamed = true"),
            "{content}"
        );
        assert!(content.starts_with("# Project policy.\n\n[unify]"), "{content}");
        assert!(!content.contains("compiler_targets"), "{content}");
        let before = effective(&decode(source.as_bytes()).unwrap().config).unwrap();
        let after = effective(&decode(content.as_bytes()).unwrap().config).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_default_only_file_is_deleted_only_when_deletion_keeps_the_defaults() {
        let source = "[unify]\ncompiler_targets = \"all\"\n\n[surface]\n";
        let migration = plan(source.as_bytes(), true).expect("migration");
        assert_eq!(migration.file, MigratedFile::Delete, "{migration:?}");
        assert_eq!(
            removed_paths(&migration),
            ["unify.compiler_targets", "unify", "surface"]
        );

        let shadowing = plan(source.as_bytes(), false).expect("migration");
        assert_eq!(shadowing.file, MigratedFile::Rewrite(String::new()));
    }

    #[test]
    fn a_header_comment_survives_when_the_first_setting_is_removed() {
        let source = "# Release policy for this repository.\ntargets = []\n\n[unify]\ninclude_renamed = true\n";
        let migration = plan(source.as_bytes(), true).expect("migration");
        assert_eq!(
            migration.file,
            MigratedFile::Rewrite("# Release policy for this repository.\n\n[unify]\ninclude_renamed = true\n".into())
        );
    }

    #[test]
    fn the_config_print_header_is_removed_with_its_defaults() {
        let source = "# Effective configuration (rail.toml)\n# This shows all settings including defaults for unset fields.\n\ntargets = []\n\n[unify]\ninclude_renamed = true\n";
        let migration = plan(source.as_bytes(), true).expect("migration");
        assert_eq!(
            migration.file,
            MigratedFile::Rewrite("[unify]\ninclude_renamed = true\n".into())
        );
        assert_eq!(migration.removed_comments.len(), 2);

        let header_only = "# Effective configuration (rail.toml)\n# This shows all settings including defaults for unset fields.\n\n[unify]\ninclude_renamed = true\n";
        let migration = plan(header_only.as_bytes(), true).expect("migration");
        assert!(migration.removed.is_empty());
        assert_eq!(
            migration.file,
            MigratedFile::Rewrite("[unify]\ninclude_renamed = true\n".into())
        );
    }

    fn migrated(source: &str) -> (ConfigMigration, String) {
        let migration = plan(source.as_bytes(), true).expect("migration");
        let MigratedFile::Rewrite(content) = migration.file.clone() else {
            panic!("expected a rewrite: {migration:?}");
        };
        decode(content.as_bytes()).expect("the migrated file decodes");
        (migration, content)
    }

    #[test]
    fn retired_release_keys_become_their_replacements() {
        // Cargo-Rail's own pre-0.32 release policy.
        let (migration, content) = migrated(
            "[release]\nauxiliary_cargo_manifests = [\"tools/driver/Cargo.toml\"]\nhosted_workflow = \".github/workflows/release.yml\"\nregistry_publication = \"crates-io\"\nremote_effects = \"github\"\ntag_format = \"{prefix}{version}\"\nvalidation = { \".github/workflows/ci.yml\" = [\"x86_64-linux\"] }\n\n[release.artifacts.cargo-rail]\nworkflow = \".github/workflows/package.yml\"\n[release.artifacts.cargo-rail.files]\n\"cargo-rail-x86_64-unknown-linux-gnu.zip\" = { target = \"x86_64-unknown-linux-gnu\" }\n\"SHA256SUMS\" = {}\n",
        );
        assert_eq!(
            content,
            "[release]\nauxiliary_cargo_manifests = [\"tools/driver/Cargo.toml\"]\ntag_format = \"v{version}\"\n\n[release.assets]\ncargo-rail = [\"cargo-rail-x86_64-unknown-linux-gnu.zip\", \"SHA256SUMS\"]\n"
        );
        let retired = migration
            .removed
            .iter()
            .filter(|setting| setting.removed_in == Some("0.32.0"))
            .map(|setting| setting.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            retired,
            [
                "release.artifacts",
                "release.hosted_workflow",
                "release.registry_publication",
                "release.remote_effects",
                "release.validation"
            ]
        );

        // A repository that pushed and validated on GitHub but never published to a registry.
        let (_, content) = migrated(
            "[release]\nremote_effects = \"github\"\nhosted_workflow = \".github/workflows/release.yml\"\n\n[release.validation]\n\".github/workflows/ci.yml\" = [\"linux\"]\n",
        );
        assert_eq!(content, "[release]\npublish = \"none\"\n");

        // Local preparation only: no push effects, a literal prefix, and a per-crate format.
        let (migration, content) = migrated(
            "[release]\nsemver_check = \"deny\"\ntag_format = \"{prefix}{version}\"\n\n[crates.lib-rustls.release]\ntag_format = \"{crate}-{prefix}{version}\"\n",
        );
        assert_eq!(
            content,
            "[release]\nsemver_check = \"deny\"\ntag_format = \"v{version}\"\npublish = \"none\"\ngithub_release = false\nchecks = \"none\"\n\n[crates.lib-rustls.release]\ntag_format = \"{crate}-v{version}\"\n"
        );
        assert!(migration.removed.is_empty(), "{migration:?}");
    }

    #[test]
    fn current_release_policy_is_never_rewritten() {
        for source in [
            "[release]\ntag_format = \"v{version}\"\n",
            "[release]\nsemver_check = \"deny\"\n",
            "[release]\npublish = \"none\"\nchecks = \"none\"\n",
        ] {
            let migration = plan(source.as_bytes(), true).expect("migration");
            assert_eq!(migration.file, MigratedFile::Unchanged, "{source}: {migration:?}");
            assert!(migration.added.is_empty());
        }
    }

    #[test]
    fn intentional_policy_is_unchanged() {
        let source = "targets = [\"x86_64-unknown-linux-gnu\"]\n\n[unify]\ncompiler_targets = \"none\"\n";
        let migration = plan(source.as_bytes(), true).expect("migration");
        assert_eq!(migration.file, MigratedFile::Unchanged, "{migration:?}");
        assert!(migration.removed.is_empty());
    }
}
