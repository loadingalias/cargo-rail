//! Sparse configuration field inventory.
//!
//! Defaults live with their typed configuration fields. This module records why
//! a field exists; it is deliberately not a second defaults registry.

use std::fmt;

/// One concrete TOML path whose keys remain distinct from TOML separators.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ConfigPath {
    segments: Vec<String>,
}

impl ConfigPath {
    pub(crate) fn root() -> Self {
        Self { segments: Vec::new() }
    }

    pub(crate) fn from_dotted(path: &str) -> Self {
        Self {
            segments: path.split('.').map(str::to_string).collect(),
        }
    }

    pub(crate) fn child(&self, segment: impl Into<String>) -> Self {
        let mut segments = self.segments.clone();
        segments.push(segment.into());
        Self { segments }
    }

    pub(crate) fn segments(&self) -> &[String] {
        &self.segments
    }

    pub(crate) fn is_root(&self) -> bool {
        self.segments.is_empty()
    }
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.segments.iter().enumerate() {
            if index > 0 {
                formatter.write_str(".")?;
            }
            if !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                formatter.write_str(segment)?;
            } else {
                let quoted = serde_json::to_string(segment).map_err(|_| fmt::Error)?;
                formatter.write_str(&quoted)?;
            }
        }
        Ok(())
    }
}

/// One leaf in the public configuration schema.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    /// Dotted TOML path. `<name>` and `<index>` match dynamic map/array keys.
    pub path: &'static str,
    /// Why changing the value changes observable behavior.
    pub why: &'static str,
    /// Built-in work that consumes this exact effective field.
    pub consumers: &'static [&'static str],
}

const fn policy(path: &'static str, why: &'static str) -> FieldSpec {
    FieldSpec {
        path,
        why,
        consumers: &[],
    }
}

const fn subscribed_policy(path: &'static str, why: &'static str, consumers: &'static [&'static str]) -> FieldSpec {
    FieldSpec { path, why, consumers }
}

/// Complete leaf-field inventory for `rail.toml`.
///
/// Dynamic tables use `<name>` and arrays of tables use `<index>`. Adding a
/// deserializable field requires adding it here or a schema coverage test fails.
pub const FIELD_SPECS: &[FieldSpec] = &[
    policy(
        "targets",
        "Selects additional Cargo target-resolution views that the repository supports.",
    ),
    policy(
        "unify.compiler_targets",
        "Selects the exact resolution-target subset where Unify acquires compiler diagnostics.",
    ),
    policy(
        "unify.include_paths",
        "Chooses whether path dependencies participate in unification.",
    ),
    policy(
        "unify.include_renamed",
        "Chooses whether renamed dependency declarations participate in unification.",
    ),
    policy(
        "unify.transitive_pinning.host",
        "Enables host-owned pins for fragmented transitive features and selects the owning manifest.",
    ),
    policy("unify.exclude", "Excludes named dependency cohorts from unification."),
    policy("unify.include", "Force-includes named dependencies in unification."),
    policy(
        "unify.max_backups",
        "Bounds retained recovery copies after manifest mutation.",
    ),
    policy(
        "unify.compiler_artifact_soft_limit_bytes",
        "Reports storage pressure when the command-owned compiler working set reaches this many bytes.",
    ),
    policy(
        "unify.compiler_artifact_hard_limit_bytes",
        "Stops compiler acquisition before its command-owned artifact working set can grow without bound.",
    ),
    policy(
        "unify.msrv_policy.mode",
        "Enables or disables workspace MSRV computation.",
    ),
    policy(
        "unify.msrv_policy.source",
        "Selects which authoritative inputs determine computed workspace MSRV.",
    ),
    policy(
        "unify.msrv_policy.inherit",
        "Chooses whether members inherit the computed workspace rust-version.",
    ),
    policy(
        "unify.consumer_scope",
        "Declares whether the workspace is the complete consumer universe for destructive pruning.",
    ),
    policy(
        "unify.preserve_features",
        "Names features that repository policy keeps even when dormant.",
    ),
    policy(
        "unify.strict_version_compat",
        "Chooses whether incompatible dependency requirements block unification.",
    ),
    policy(
        "unify.exact_pin_handling",
        "Chooses how exact dependency pins are preserved or rejected.",
    ),
    policy(
        "unify.major_version_conflict",
        "Chooses whether major-version conflicts remain split or are explicitly bumped.",
    ),
    policy(
        "unify.skip_undeclared_patterns",
        "Declares borrowed-feature names that are intentionally non-actionable.",
    ),
    policy(
        "release.source",
        "Selects release input or reviewed presentation policy.",
    ),
    policy(
        "release.require_changelog_entries",
        "Selects release input or reviewed presentation policy.",
    ),
    policy(
        "release.require_release_notes",
        "Selects release input or reviewed presentation policy.",
    ),
    policy(
        "release.release_notes_dir",
        "Selects release input or reviewed presentation policy.",
    ),
    policy(
        "release.unconventional_commits",
        "Selects release input or reviewed presentation policy.",
    ),
    policy(
        "release.require_change_files",
        "Selects release input or reviewed presentation policy.",
    ),
    policy(
        "release.changelog.entry_format",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.emoji",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.group_order",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.fallback",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.groups",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.groups.<index>.types",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.groups.<index>.title",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.groups.<index>.emoji",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.filters.skip_types",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.filters.skip_scopes",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.filters.include_paths",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.filters.exclude_paths",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.commit_url",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "release.changelog.pr_url",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.entry_format",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.emoji",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.group_order",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.fallback",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.groups",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.groups.<index>.types",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.groups.<index>.title",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.groups.<index>.emoji",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.filters.skip_types",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.filters.skip_scopes",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.filters.include_paths",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.filters.exclude_paths",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.commit_url",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy(
        "crates.<name>.changelog.pr_url",
        "Controls changelog presentation and commit attribution without granting publication authority.",
    ),
    policy("release.tag_prefix", "Defines the repository's release tag prefix."),
    policy(
        "release.tag_format",
        "Defines the repository's crate/version tag namespace.",
    ),
    policy(
        "release.remote_effects",
        "Selects one valid remote-effect boundary: none, push, auto, GitHub, or GitLab.",
    ),
    policy(
        "release.registry_publication",
        "Selects an exact package-registry publication boundary independently from Git and forge effects.",
    ),
    policy("release.sign_tags", "Requires cryptographic signing of release tags."),
    policy(
        "release.change_dir",
        "Selects the repository directory containing reviewed release intent.",
    ),
    policy(
        "release.pre_1_breaking_bump",
        "Defines how reviewed breaking intent maps onto pre-1.0 versions.",
    ),
    subscribed_policy(
        "release.semver_check",
        "Selects how cargo-semver-checks evidence gates a release.",
        &["release.semver"],
    ),
    policy(
        "release.version_groups",
        "Defines named crate sets that version and release in lockstep.",
    ),
    policy(
        "release.version_groups.<name>",
        "Defines crates that version and release in lockstep.",
    ),
    policy(
        "release.auxiliary_cargo_manifests",
        "Names standalone Cargo manifests whose committed lockfiles are exact release projections.",
    ),
    policy("release.changelog.path", "Defines the default changelog path."),
    policy(
        "release.changelog.relative_to",
        "Defines the root used to resolve changelog paths.",
    ),
    policy(
        "surface.enabled",
        "Chooses whether the planner selects source-surface analysis for relevant repository changes.",
    ),
    policy(
        "surface.consumer_scope",
        "Declares whether the workspace is the complete consumer universe for internal source visibility.",
    ),
    policy(
        "surface.targets",
        "Selects an explicit host/configured-target subset or inherits the top-level workspace target policy.",
    ),
    policy(
        "surface.crate_visibility",
        "Chooses whether pub(crate) to pub(super) findings are enabled.",
    ),
    policy(
        "surface.preserve_uniform_fields",
        "Chooses whether visibility repair preserves uniform field visibility within a declaration.",
    ),
    policy(
        "surface.product",
        "Defines the complete set of shipped production roots.",
    ),
    policy(
        "surface.product.<index>.package",
        "Selects the exact workspace package that owns a production root.",
    ),
    policy(
        "surface.product.<index>.bin",
        "Selects one shipped Cargo binary target as a production root.",
    ),
    policy(
        "surface.product.<index>.lib",
        "Selects one shipped Cargo library target as a public production root.",
    ),
    policy(
        "surface.product.<index>.target",
        "Restricts a production root to one Cargo target selector.",
    ),
    policy(
        "surface.product.<index>.reason",
        "Records why the selected target belongs to the complete production root set.",
    ),
    policy("surface.lint", "Defines ordered workspace-wide surface lint levels."),
    policy(
        "surface.lint.<index>.selector",
        "Selects one exact surface lint or the warnings group.",
    ),
    policy(
        "surface.lint.<index>.level",
        "Sets the ordered disposition for the selected lint group.",
    ),
    policy(
        "surface.feature-profile",
        "Defines the exact Cargo feature views included in source-surface analysis.",
    ),
    policy(
        "surface.feature-profile.<index>.name",
        "Names one stable source-surface feature view.",
    ),
    policy(
        "surface.feature-profile.<index>.all-features",
        "Selects every declared Cargo feature for one source-surface view.",
    ),
    policy(
        "surface.feature-profile.<index>.no-default-features",
        "Disables Cargo default features for one source-surface view.",
    ),
    policy(
        "surface.feature-profile.<index>.features",
        "Selects exact Cargo features for one source-surface view.",
    ),
    policy(
        "surface.doctest",
        "Defines the exact workspace packages whose doctests are compiled.",
    ),
    policy(
        "surface.doctest.<index>.package",
        "Selects one doctest-enabled workspace package.",
    ),
    policy(
        "surface.doctest_coverage",
        "Chooses automatic or disabled doctest coverage when no exact package list exists.",
    ),
    policy(
        "surface.external",
        "Keeps explicitly external compiler crates outside closed-world authority.",
    ),
    policy(
        "surface.external.<index>.crate",
        "Selects one exact Rust compiler crate with external consumers.",
    ),
    policy(
        "surface.external.<index>.reason",
        "Records why the compiler crate remains open to external consumers.",
    ),
    policy("surface.override", "Defines item-specific surface diagnostic policy."),
    policy(
        "surface.override.<index>.lint",
        "Selects the exact surface lint to override.",
    ),
    policy(
        "surface.override.<index>.package",
        "Selects the exact workspace package owning the overridden declaration.",
    ),
    policy(
        "surface.override.<index>.crate",
        "Selects the exact Rust compiler crate owning the overridden declaration.",
    ),
    policy(
        "surface.override.<index>.item",
        "Selects the declaration by its compiler diagnostic path.",
    ),
    policy(
        "surface.override.<index>.kind",
        "Disambiguates the selected declaration by its exact Rust item kind.",
    ),
    policy(
        "surface.override.<index>.target",
        "Restricts an item override to one Cargo target selector.",
    ),
    policy(
        "surface.override.<index>.level",
        "Keeps, suppresses, or expects the selected surface finding.",
    ),
    policy(
        "surface.override.<index>.reason",
        "Records why the item-specific surface policy exists.",
    ),
    policy(
        "surface.exclude",
        "Excludes one module or source file from surface diagnostics.",
    ),
    policy(
        "surface.exclude.<index>.package",
        "Selects the exact workspace package owning the excluded scope.",
    ),
    policy(
        "surface.exclude.<index>.crate",
        "Selects the exact Rust compiler crate owning the excluded scope.",
    ),
    policy(
        "surface.exclude.<index>.module",
        "Selects an excluded compiler diagnostic module path.",
    ),
    policy(
        "surface.exclude.<index>.file",
        "Selects an excluded repository-relative source file.",
    ),
    policy(
        "surface.exclude.<index>.target",
        "Restricts an excluded scope to one Cargo target selector.",
    ),
    policy(
        "surface.exclude.<index>.level",
        "Suppresses or expects findings in the excluded scope.",
    ),
    policy(
        "surface.exclude.<index>.reason",
        "Records why the excluded source scope exists.",
    ),
    policy("plan.work", "Declares input-only repository work."),
    policy(
        "plan.work.<name>.scope",
        "Selects the typed scope emitted for declared work.",
    ),
    policy(
        "plan.work.<name>.paths",
        "Declares positive repository-relative path inputs.",
    ),
    policy(
        "plan.work.<name>.config",
        "Declares exact effective configuration inputs.",
    ),
    policy(
        "plan.work.<name>.cargo",
        "Subscribes declared work to code-owned Cargo work.",
    ),
    policy(
        "plan.work.<name>.variant_catalog",
        "Selects a checked-in declarative variant catalog.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites",
        "Declares bounded one-hop Cargo artifact prerequisites for this named work item.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.source_work",
        "Names the code-owned Cargo execution work that activates this prerequisite edge.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.when",
        "Selects source packages or exact targets that activate this prerequisite edge.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.when.<index>.package",
        "Names an exact source workspace package.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.when.<index>.target",
        "Selects an optional exact source Cargo target.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.when.<index>.target.name",
        "Names the exact source Cargo target.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.when.<index>.target.kind",
        "Names one exact source Cargo target kind.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.require",
        "Selects prerequisite packages or exact targets emitted by this named work item.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.require.<index>.package",
        "Names an exact prerequisite workspace package.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.require.<index>.target",
        "Selects an optional exact prerequisite Cargo target.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.require.<index>.target.name",
        "Names the exact prerequisite Cargo target.",
    ),
    policy(
        "plan.work.<name>.cargo_prerequisites.<index>.require.<index>.target.kind",
        "Names one exact prerequisite Cargo target kind.",
    ),
    policy("crates", "Defines per-crate policy overrides."),
    policy(
        "crates.<name>.split.remote",
        "Selects the split repository for an owned crate.",
    ),
    policy(
        "crates.<name>.split.branch",
        "Selects the destination branch for split/sync history.",
    ),
    policy(
        "crates.<name>.split.mode",
        "Selects single-crate or combined-repository splitting.",
    ),
    policy(
        "crates.<name>.split.workspace_mode",
        "Selects combined split repository layout.",
    ),
    policy(
        "crates.<name>.split.members",
        "Names Cargo workspace members whose snapshot-derived roots are owned by a split.",
    ),
    policy(
        "crates.<name>.split.include",
        "Declares explicit non-Cargo assets owned by a split.",
    ),
    policy(
        "crates.<name>.split.exclude",
        "Excludes paths from an otherwise owned split tree.",
    ),
    policy(
        "crates.<name>.release.publish",
        "Overrides whether a crate participates in registry publication.",
    ),
    policy("crates.<name>.changelog.path", "Overrides a crate's changelog path."),
    policy(
        "crates.<name>.changelog.relative_to",
        "Overrides a crate's changelog path root.",
    ),
    policy(
        "crates.<name>.changelog.skip",
        "Excludes a crate from changelog generation.",
    ),
];

/// Look up metadata for an exact or dynamic dotted path.
pub fn field_spec(path: &str) -> Option<&'static FieldSpec> {
    field_spec_path(&ConfigPath::from_dotted(path))
}

pub(crate) fn field_spec_path(path: &ConfigPath) -> Option<&'static FieldSpec> {
    FIELD_SPECS.iter().find(|spec| path_matches(spec.path, path))
}

/// Return built-in work consumers for one exact effective field.
pub fn field_consumers(path: &str) -> &'static [&'static str] {
    const CARGO_TARGET_CONSUMERS: &[&str] = &[
        "cargo.build",
        "cargo.clippy",
        "cargo.doc",
        "cargo.doctest",
        "cargo.package",
        "cargo.test",
        "surface",
    ];
    const SURFACE_CONSUMERS: &[&str] = &["surface"];
    let Some(spec) = field_spec(path) else {
        return &[];
    };
    if !spec.consumers.is_empty() {
        spec.consumers
    } else if spec.path == "targets" {
        CARGO_TARGET_CONSUMERS
    } else if spec.path.starts_with("surface.") {
        SURFACE_CONSUMERS
    } else {
        &[]
    }
}

pub(crate) fn is_known_config_path(path: &ConfigPath) -> bool {
    field_spec_path(path).is_some() || FIELD_SPECS.iter().any(|spec| path_prefix_matches(spec.path, path))
}

fn path_matches(pattern: &str, path: &ConfigPath) -> bool {
    let pattern = pattern.split('.');
    pattern.clone().count() == path.segments().len()
        && pattern
            .zip(path.segments())
            .all(|(expected, actual)| expected.starts_with('<') || expected == actual)
}

fn path_prefix_matches(pattern: &str, path: &ConfigPath) -> bool {
    let pattern = pattern.split('.');
    path.segments().len() < pattern.clone().count()
        && path
            .segments()
            .iter()
            .zip(pattern)
            .all(|(actual, expected)| expected.starts_with('<') || actual == expected)
}

pub(crate) fn document_paths(doc: &toml_edit::DocumentMut) -> Vec<ConfigPath> {
    let mut paths = Vec::new();
    collect_table_like_paths(doc.as_table(), &ConfigPath::root(), &mut paths);
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn collect_table_like_paths(table: &dyn toml_edit::TableLike, prefix: &ConfigPath, paths: &mut Vec<ConfigPath>) {
    for (key, item) in table.iter() {
        let path = prefix.child(key);
        paths.push(path.clone());

        if let Some(child) = item.as_table_like() {
            collect_table_like_paths(child, &path, paths);
        } else if let Some(array) = item.as_array_of_tables() {
            for (index, child) in array.iter().enumerate() {
                collect_table_like_paths(child, &path.child(index.to_string()), paths);
            }
        } else if let Some(array) = item.as_array() {
            for (index, value) in array.iter().enumerate() {
                if let Some(child) = value.as_inline_table() {
                    collect_table_like_paths(child, &path.child(index.to_string()), paths);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_paths_match_schema_inventory() {
        assert_eq!(
            field_spec("crates.demo.split.remote").map(|field| field.path),
            Some("crates.<name>.split.remote")
        );
    }

    #[test]
    fn structural_paths_preserve_dynamic_keys() {
        let doc: toml_edit::DocumentMut = r#"
[plan.work."docs.generated"]
paths = ["docs/**"]

[crates."cli-tools".release]
publish = false
"#
        .parse()
        .unwrap();
        let paths = document_paths(&doc);
        assert!(
            paths
                .iter()
                .any(|path| path.to_string() == "plan.work.\"docs.generated\".paths")
        );
        assert!(
            paths
                .iter()
                .any(|path| path.to_string() == "crates.cli-tools.release.publish")
        );
        assert!(paths.iter().all(is_known_config_path));
    }

    #[test]
    fn structural_paths_descend_inline_tables_and_inline_table_arrays() {
        let doc: toml_edit::DocumentMut = r#"
release = { changelog = { path = "CHANGELOG.md", removed = true } }
surface = { product = [{ package = "cargo-rail", bin = "cargo-rail", removed = true }] }
"#
        .parse()
        .unwrap();
        let paths = document_paths(&doc)
            .into_iter()
            .map(|path| path.to_string())
            .collect::<Vec<_>>();

        assert!(paths.iter().any(|path| path == "release.changelog.path"));
        assert!(paths.iter().any(|path| path == "release.changelog.removed"));
        assert!(paths.iter().any(|path| path == "surface.product.0.package"));
        assert!(paths.iter().any(|path| path == "surface.product.0.removed"));
    }

    #[test]
    fn field_consumers_are_owned_by_the_schema_inventory() {
        assert_eq!(field_consumers("release.semver_check"), &["release.semver"]);
        assert_eq!(field_consumers("surface.enabled"), &["surface"]);
        assert!(field_consumers("release.tag_format").is_empty());
    }

    #[test]
    fn inventory_paths_are_unique() {
        let mut paths: Vec<_> = FIELD_SPECS.iter().map(|field| field.path).collect();
        paths.sort_unstable();
        let count = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), count);
    }

    #[test]
    fn every_typed_default_leaf_is_classified() {
        fn visit(value: &serde_json::Value, path: &str, missing: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(object) if !object.is_empty() => {
                    for (key, value) in object {
                        let child = if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{path}.{key}")
                        };
                        visit(value, &child, missing);
                    }
                }
                _ if !path.is_empty() && field_spec(path).is_none() => missing.push(path.to_string()),
                _ => {}
            }
        }

        let defaults = serde_json::to_value(crate::config::RailConfig::default()).expect("serialize defaults");
        let mut missing = Vec::new();
        visit(&defaults, "", &mut missing);
        assert!(missing.is_empty(), "unclassified configuration fields: {missing:?}");
    }
}
