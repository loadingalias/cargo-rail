//! Sparse configuration field inventory.
//!
//! Defaults live with their typed configuration fields. This module records why
//! a field exists; it is deliberately not a second defaults registry.

/// Why a configuration input exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldClassification {
  /// A competent repository may intentionally choose a different value.
  ProjectPolicy,
  /// An old spelling accepted only during a bounded migration window.
  CompatibilityInput,
  /// An internal behavior that should never have been configurable.
  ImplementationDetail,
}

impl FieldClassification {
  /// Stable text/JSON spelling.
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::ProjectPolicy => "project_policy",
      Self::CompatibilityInput => "compatibility_input",
      Self::ImplementationDetail => "implementation_detail",
    }
  }
}

/// One leaf in the public or compatibility configuration schema.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
  /// Dotted TOML path. `<name>` and `<index>` match dynamic map/array keys.
  pub path: &'static str,
  /// Architectural classification.
  pub classification: FieldClassification,
  /// Why changing the value changes observable behavior.
  pub why: &'static str,
  /// Deprecation and migration guidance, when applicable.
  pub deprecation: Option<&'static str>,
}

const fn policy(path: &'static str, why: &'static str) -> FieldSpec {
  FieldSpec {
    path,
    classification: FieldClassification::ProjectPolicy,
    why,
    deprecation: None,
  }
}

const fn compatibility(path: &'static str, why: &'static str, deprecation: &'static str) -> FieldSpec {
  FieldSpec {
    path,
    classification: FieldClassification::CompatibilityInput,
    why,
    deprecation: Some(deprecation),
  }
}

const fn implementation(path: &'static str, deprecation: &'static str) -> FieldSpec {
  FieldSpec {
    path,
    classification: FieldClassification::ImplementationDetail,
    why: "Correctness and deterministic output are cargo-rail responsibilities, not repository policy.",
    deprecation: Some(deprecation),
  }
}

const LEGACY_UNKNOWN_FILE_POLICY_BOOL: FieldSpec = compatibility(
  "change-detection.unknown_file_policy",
  "Preserves the legacy boolean value until it is explicitly migrated.",
  "Deprecated: boolean unknown-file policy values must be migrated to `docs` or `owned_build_test` with `cargo rail config migrate`.",
);

/// Complete leaf-field inventory for `rail.toml`.
///
/// Dynamic tables use `<name>` and arrays of tables use `<index>`. Adding a
/// deserializable field requires adding it here or a schema coverage test fails.
pub const FIELD_SPECS: &[FieldSpec] = &[
  policy(
    "targets",
    "Selects additional Cargo target-resolution views that the repository supports.",
  ),
  implementation(
    "workspace",
    "Deprecated: this reserved table had no behavior. Run `cargo rail config migrate` to remove it.",
  ),
  implementation(
    "toolchain",
    "Deprecated: this reserved table had no behavior. Run `cargo rail config migrate` to remove it.",
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
    "Enables workspace-hack-style transitive pinning and selects its owning manifest.",
  ),
  compatibility(
    "unify.pin_transitives",
    "Preserves the old enable boolean until the pinning policy is migrated.",
    "Deprecated: migrate to `unify.transitive_pinning` with `cargo rail config migrate`.",
  ),
  compatibility(
    "unify.transitive_host",
    "Preserves the old host field until the pinning policy is migrated.",
    "Deprecated: migrate to `unify.transitive_pinning` with `cargo rail config migrate`.",
  ),
  policy("unify.exclude", "Excludes named dependency cohorts from unification."),
  policy("unify.include", "Force-includes named dependencies in unification."),
  policy(
    "unify.max_backups",
    "Bounds retained recovery copies after manifest mutation.",
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
  compatibility(
    "unify.msrv",
    "Preserves the old enable boolean until MSRV policy is migrated.",
    "Deprecated: migrate to `unify.msrv_policy` with `cargo rail config migrate`.",
  ),
  compatibility(
    "unify.enforce_msrv_inheritance",
    "Preserves the old inheritance boolean until MSRV policy is migrated.",
    "Deprecated: migrate to `unify.msrv_policy` with `cargo rail config migrate`.",
  ),
  compatibility(
    "unify.msrv_source",
    "Preserves the old source selector until MSRV policy is migrated.",
    "Deprecated: migrate to `unify.msrv_policy` with `cargo rail config migrate`.",
  ),
  compatibility(
    "unify.prune_dead_features",
    "Preserves the old analysis toggle during its migration window.",
    "Deprecated: dead-feature diagnostics are unconditional and deletion requires `consumer_scope = \"workspace\"`. Run `cargo rail config migrate` to remove this field.",
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
  compatibility(
    "unify.detect_unused",
    "Preserves the old analysis toggle during its migration window.",
    "Deprecated: unused-dependency diagnostics are unconditional. Run `cargo rail config migrate` to remove this field.",
  ),
  implementation(
    "unify.compiler_diag_cache",
    "Deprecated: compiler evidence caching is automatic. Run `cargo rail config migrate` to remove this field.",
  ),
  compatibility(
    "unify.remove_unused",
    "Preserves the old edit toggle during its migration window.",
    "Deprecated: `unify --check` is read-only and unify apply owns proven edits. Run `cargo rail config migrate` to remove this field.",
  ),
  compatibility(
    "unify.detect_undeclared_features",
    "Preserves the old analysis toggle during its migration window.",
    "Deprecated: borrowed-feature diagnostics are unconditional. Run `cargo rail config migrate` to remove this field.",
  ),
  compatibility(
    "unify.fix_undeclared_features",
    "Preserves the old edit toggle during its migration window.",
    "Deprecated: `unify --check` is read-only and unify apply owns proven edits. Run `cargo rail config migrate` to remove this field.",
  ),
  policy(
    "unify.skip_undeclared_patterns",
    "Declares borrowed-feature names that are intentionally non-actionable.",
  ),
  implementation(
    "unify.sort_dependencies",
    "Deprecated: dependency edits are always deterministic. Run `cargo rail config migrate` to remove this field.",
  ),
  policy(
    "release.source",
    "Selects reviewed changes or an explicit conventional-commit compatibility mode as release input.",
  ),
  policy("release.tag_prefix", "Defines the repository's release tag prefix."),
  policy(
    "release.tag_format",
    "Defines the repository's crate/version tag namespace.",
  ),
  compatibility(
    "release.require_clean",
    "Preserves old configuration while release apply moves to exact planned-input cleanliness.",
    "Deprecated: previews permit dirt and apply always rejects paths outside the bound plan. Run `cargo rail config migrate` to remove this field.",
  ),
  compatibility(
    "release.publish_delay",
    "Preserves old configuration after cargo-rail stopped polling registry convergence.",
    "Deprecated: release execution never delays between publishes; it stops at registry wait boundaries and resumes by reconciliation. Run `cargo rail config migrate` to remove this field.",
  ),
  policy(
    "release.remote_effects",
    "Selects one valid remote-effect boundary: none, push, auto, GitHub, or GitLab.",
  ),
  compatibility(
    "release.create_github_release",
    "Preserves the old forge-release boolean until its effect matrix is migrated.",
    "Deprecated: migrate release remote effects to `release.remote_effects` with `cargo rail config migrate`.",
  ),
  compatibility(
    "release.forge",
    "Preserves the old provider selector until its effect matrix is migrated.",
    "Deprecated: migrate release remote effects to `release.remote_effects` with `cargo rail config migrate`.",
  ),
  compatibility(
    "release.push",
    "Preserves the old push boolean until its effect matrix is migrated.",
    "Deprecated: migrate release remote effects to `release.remote_effects` with `cargo rail config migrate`.",
  ),
  policy("release.sign_tags", "Requires cryptographic signing of release tags."),
  policy(
    "release.require_changelog_entries",
    "Requires generated changelog content for each released crate.",
  ),
  policy(
    "release.require_release_notes",
    "Requires reviewed release notes before remote effects.",
  ),
  policy(
    "release.release_notes_dir",
    "Selects the repository directory for manual release notes.",
  ),
  policy(
    "release.change_dir",
    "Selects the repository directory containing reviewed release intent.",
  ),
  policy(
    "release.pre_1_breaking_bump",
    "Defines how reviewed breaking intent maps onto pre-1.0 versions.",
  ),
  policy(
    "release.unconventional_commits",
    "Defines compatibility handling for commit messages outside the reviewed intent model.",
  ),
  policy(
    "release.semver_check",
    "Selects how cargo-semver-checks evidence gates a release.",
  ),
  policy(
    "release.require_change_files",
    "Selects crates that require reviewed change-file coverage.",
  ),
  policy(
    "release.version_groups",
    "Defines named crate sets that version and release in lockstep.",
  ),
  policy(
    "release.version_groups.<name>",
    "Defines crates that version and release in lockstep.",
  ),
  policy("release.changelog.path", "Defines the default changelog path."),
  policy(
    "release.changelog.relative_to",
    "Defines the root used to resolve changelog paths.",
  ),
  policy(
    "release.changelog.entry_format",
    "Defines the bounded changelog entry rendering shape.",
  ),
  policy(
    "release.changelog.emoji",
    "Chooses whether changelog section headings include emoji.",
  ),
  policy(
    "release.changelog.group_order",
    "Defines deterministic changelog section ordering.",
  ),
  policy(
    "release.changelog.fallback",
    "Defines handling for unlisted change types.",
  ),
  policy(
    "release.changelog.groups",
    "Defines project-specific changelog sections.",
  ),
  policy(
    "release.changelog.groups.<index>.types",
    "Maps project-specific change types to a section.",
  ),
  policy(
    "release.changelog.groups.<index>.title",
    "Defines a project-specific changelog section title.",
  ),
  policy(
    "release.changelog.groups.<index>.emoji",
    "Defines a project-specific changelog section emoji.",
  ),
  policy(
    "release.changelog.filters.skip_types",
    "Excludes reviewed change types from changelog output.",
  ),
  policy(
    "release.changelog.filters.skip_scopes",
    "Excludes reviewed scopes from changelog output.",
  ),
  policy(
    "release.changelog.filters.include_paths",
    "Narrows changelog attribution to repository path globs.",
  ),
  policy(
    "release.changelog.filters.exclude_paths",
    "Excludes repository path globs from changelog attribution.",
  ),
  policy(
    "release.changelog.commit_url",
    "Overrides the derived commit-link format.",
  ),
  policy(
    "release.changelog.pr_url",
    "Overrides the derived pull-request-link format.",
  ),
  policy(
    "change-detection.infrastructure",
    "Defines repository paths that invalidate workspace-wide infrastructure.",
  ),
  policy(
    "change-detection.custom",
    "Defines project-specific planner output categories.",
  ),
  policy(
    "change-detection.custom.<name>",
    "Defines a project-specific planner output category.",
  ),
  policy(
    "change-detection.unknown_file_policy",
    "Defines conservative impact for otherwise unclassified paths.",
  ),
  policy(
    "change-detection.confidence_profile",
    "Selects the repository's default planner safety profile.",
  ),
  compatibility(
    "change-detection.conservative_unclassified_owner_fallback",
    "Preserves the old boolean spelling until it is explicitly migrated.",
    "Deprecated: migrate to `change-detection.unknown_file_policy` with `cargo rail config migrate`.",
  ),
  compatibility(
    "change-detection.bot_pr_confidence_profile",
    "Preserves an old provider-specific planner override during its compatibility window.",
    "Deprecated: provider identity no longer changes planner policy. Run `cargo rail config migrate` to remove it.",
  ),
  policy(
    "run.default_profile",
    "Selects the named action profile used when the CLI does not choose one.",
  ),
  policy("run.profile", "Defines repository-specific named action profiles."),
  policy(
    "run.profile.<name>.actions",
    "Selects ordered built-in or repository action IDs for a named profile.",
  ),
  compatibility(
    "run.profile.<name>.surfaces",
    "Preserves the old executable-surface spelling during the action migration.",
    "Deprecated: migrate to `run.profile.<name>.actions` with `cargo rail config migrate`.",
  ),
  policy(
    "run.profile.<name>.run_args",
    "Adds literal Cargo/test arguments to a named profile.",
  ),
  policy(
    "run.profile.<name>.baseline.kind",
    "Selects one baseline mode for a named profile.",
  ),
  policy(
    "run.profile.<name>.baseline.reference",
    "Selects the explicit reference used by a since baseline.",
  ),
  compatibility(
    "run.profile.<name>.since",
    "Preserves the old since field until its baseline is migrated.",
    "Deprecated: migrate to the typed run profile `baseline` with `cargo rail config migrate`.",
  ),
  compatibility(
    "run.profile.<name>.merge_base",
    "Preserves the old merge-base boolean until its baseline is migrated.",
    "Deprecated: migrate to the typed run profile `baseline` with `cargo rail config migrate`.",
  ),
  policy("run.workflow", "Defines repository workflow-to-profile mappings."),
  policy(
    "run.workflow.<name>",
    "Maps a repository workflow name to a named profile.",
  ),
  policy("run.action", "Defines bounded direct-argv repository actions."),
  policy(
    "run.action.<name>.kind",
    "Distinguishes ordinary tasks from generated-output owners.",
  ),
  policy(
    "run.action.<name>.argv",
    "Defines the direct executable and literal regeneration argument vector.",
  ),
  policy(
    "run.action.<name>.check_argv",
    "Defines the read-only staleness check for generated outputs.",
  ),
  policy(
    "run.action.<name>.dependencies",
    "Orders prerequisites in the deterministic action graph.",
  ),
  policy(
    "run.action.<name>.when",
    "Maps planner impact surfaces to repository action selection.",
  ),
  policy(
    "run.action.<name>.working_directory",
    "Selects a repository-contained logical working directory.",
  ),
  policy(
    "run.action.<name>.packages",
    "Selects typed planner-package insertion behavior.",
  ),
  policy("run.action.<name>.targets", "Declares explicit Cargo target arguments."),
  policy(
    "run.action.<name>.features",
    "Declares the feature domain represented by the action.",
  ),
  policy(
    "run.action.<name>.inputs",
    "Declares repository input scopes for the action contract.",
  ),
  policy(
    "run.action.<name>.outputs",
    "Declares repository outputs owned by a generated action.",
  ),
  policy(
    "run.action.<name>.environment.inherit",
    "Chooses whether a repository action inherits ambient environment state.",
  ),
  policy(
    "run.action.<name>.environment.entries",
    "Defines typed fixed, pass-through, Cargo-derived, and secret environment entries.",
  ),
  policy(
    "run.action.<name>.environment.entries.<index>.kind",
    "Selects one typed environment entry variant.",
  ),
  policy(
    "run.action.<name>.environment.entries.<index>.name",
    "Names the environment capability without exposing secret values.",
  ),
  policy(
    "run.action.<name>.environment.entries.<index>.value",
    "Defines a fixed non-secret value or a Cargo-derived value source.",
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
  compatibility(
    "crates.<name>.split.paths",
    "Preserves legacy path-selected split members until explicit migration.",
    "Deprecated: split ownership is derived from Cargo member names. Run `cargo rail config migrate` to replace `paths` with `members`.",
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
  policy(
    "crates.<name>.changelog.entry_format",
    "Overrides a crate's changelog entry rendering.",
  ),
  policy(
    "crates.<name>.changelog.emoji",
    "Overrides emoji rendering for a crate changelog.",
  ),
  policy(
    "crates.<name>.changelog.group_order",
    "Overrides section order for a crate changelog.",
  ),
  policy(
    "crates.<name>.changelog.fallback",
    "Overrides unlisted-type handling for a crate changelog.",
  ),
  policy(
    "crates.<name>.changelog.groups",
    "Defines crate-specific changelog sections.",
  ),
  policy(
    "crates.<name>.changelog.groups.<index>.types",
    "Extends a crate's project-specific change types.",
  ),
  policy(
    "crates.<name>.changelog.groups.<index>.title",
    "Defines a crate-specific changelog section title.",
  ),
  policy(
    "crates.<name>.changelog.groups.<index>.emoji",
    "Defines a crate-specific changelog section emoji.",
  ),
  policy(
    "crates.<name>.changelog.filters.skip_types",
    "Overrides skipped change types for a crate.",
  ),
  policy(
    "crates.<name>.changelog.filters.skip_scopes",
    "Overrides skipped scopes for a crate.",
  ),
  policy(
    "crates.<name>.changelog.filters.include_paths",
    "Overrides included attribution paths for a crate.",
  ),
  policy(
    "crates.<name>.changelog.filters.exclude_paths",
    "Overrides excluded attribution paths for a crate.",
  ),
  policy(
    "crates.<name>.changelog.commit_url",
    "Overrides commit links for a crate changelog.",
  ),
  policy(
    "crates.<name>.changelog.pr_url",
    "Overrides pull-request links for a crate changelog.",
  ),
  implementation(
    "crates.<name>.sync",
    "Deprecated: the empty reserved table had no behavior. Run `cargo rail config migrate` to remove it.",
  ),
];

/// Look up metadata for an exact or dynamic dotted path.
pub fn field_spec(path: &str) -> Option<&'static FieldSpec> {
  FIELD_SPECS.iter().find(|spec| path_matches(spec.path, path))
}

/// Whether a concrete path is a classified field or a container leading to one.
pub fn is_known_path(path: &str) -> bool {
  field_spec(path).is_some() || FIELD_SPECS.iter().any(|spec| path_prefix_matches(spec.path, path))
}

fn path_matches(pattern: &str, path: &str) -> bool {
  let pattern = pattern.split('.');
  let path = path.split('.');
  pattern.clone().count() == path.clone().count()
    && pattern
      .zip(path)
      .all(|(expected, actual)| expected.starts_with('<') || expected == actual)
}

fn path_prefix_matches(pattern: &str, path: &str) -> bool {
  let pattern = pattern.split('.');
  let path = path.split('.');
  path.clone().count() < pattern.clone().count()
    && path
      .zip(pattern)
      .all(|(actual, expected)| expected.starts_with('<') || actual == expected)
}

/// A deprecated field found in a concrete document.
#[derive(Debug)]
pub struct PresentDeprecation {
  /// Concrete dotted path from the document.
  pub path: String,
  /// Inventory entry that matched the path.
  pub spec: &'static FieldSpec,
}

/// Find deprecated compatibility inputs without interpreting their values.
pub fn present_deprecations(doc: &toml_edit::DocumentMut) -> Vec<PresentDeprecation> {
  let mut paths = Vec::new();
  collect_table_paths(doc.as_table(), "", &mut paths);
  paths.sort_unstable();
  paths.dedup();
  let mut deprecations: Vec<_> = paths
    .into_iter()
    .filter_map(|path| {
      let spec = field_spec(&path)?;
      spec.deprecation.map(|_| PresentDeprecation { path, spec })
    })
    .collect();
  if doc
    .get("change-detection")
    .and_then(toml_edit::Item::as_table)
    .and_then(|table| table.get("unknown_file_policy"))
    .and_then(toml_edit::Item::as_bool)
    .is_some()
  {
    deprecations.push(PresentDeprecation {
      path: LEGACY_UNKNOWN_FILE_POLICY_BOOL.path.to_string(),
      spec: &LEGACY_UNKNOWN_FILE_POLICY_BOOL,
    });
  }
  deprecations.sort_unstable_by(|left, right| left.path.cmp(&right.path));
  deprecations
}

fn collect_table_paths(table: &toml_edit::Table, prefix: &str, paths: &mut Vec<String>) {
  for (key, item) in table {
    let path = if prefix.is_empty() {
      key.to_string()
    } else {
      format!("{prefix}.{key}")
    };
    paths.push(path.clone());

    if let Some(child) = item.as_table() {
      collect_table_paths(child, &path, paths);
    } else if let Some(array) = item.as_array_of_tables() {
      for (index, child) in array.iter().enumerate() {
        collect_table_paths(child, &format!("{path}.{index}"), paths);
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
    assert_eq!(
      field_spec("release.changelog.groups.0.title").map(|field| field.path),
      Some("release.changelog.groups.<index>.title")
    );
    assert!(is_known_path("run.profile.ci"));
    assert!(!is_known_path("run.profile.ci.unknown"));
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

  #[test]
  fn deprecated_fields_are_found_in_concrete_documents() {
    let doc: toml_edit::DocumentMut = r#"
[unify]
compiler_diag_cache = false

[crates.demo.sync]
"#
    .parse()
    .expect("valid fixture");
    let paths: Vec<_> = present_deprecations(&doc)
      .into_iter()
      .map(|deprecation| deprecation.path)
      .collect();
    assert_eq!(
      paths,
      vec!["crates.demo.sync".to_string(), "unify.compiler_diag_cache".to_string()]
    );
  }

  #[test]
  fn legacy_boolean_unknown_file_policy_is_value_deprecated() {
    let doc: toml_edit::DocumentMut = "[change-detection]\nunknown_file_policy = true\n"
      .parse()
      .expect("valid config");
    let deprecations = present_deprecations(&doc);
    assert_eq!(deprecations.len(), 1);
    assert_eq!(deprecations[0].path, "change-detection.unknown_file_policy");
    assert_eq!(
      deprecations[0].spec.classification,
      FieldClassification::CompatibilityInput
    );
  }
}
