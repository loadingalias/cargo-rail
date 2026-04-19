//! Configuration schema and syncable field registry
//!
//! Defines the canonical list of all configuration fields with their
//! defaults. Used by `config sync` to add missing fields to existing
//! rail.toml files without overwriting user values.

/// Specification for a syncable configuration field
#[derive(Debug, Clone)]
pub struct FieldSpec {
  /// Section name (e.g., "unify", "release", "change-detection")
  pub section: &'static str,
  /// Field key within the section
  pub key: &'static str,
  /// Default value as a TOML literal string
  pub default_toml: &'static str,
  /// Inline comment to add when inserting the field
  pub comment: &'static str,
}

/// All syncable configuration fields
///
/// These are the fields that `cargo rail config sync` will add
/// if missing from an existing rail.toml. The order here determines
/// the insertion order within each section.
pub const SYNCABLE_FIELDS: &[FieldSpec] = &[
  // =========================================================================
  // [unify] section - 22 fields
  // =========================================================================
  FieldSpec {
    section: "unify",
    key: "include_paths",
    default_toml: "true",
    comment: "Handle path dependencies (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "include_renamed",
    default_toml: "false",
    comment: "Handle renamed dependencies (default: false)",
  },
  FieldSpec {
    section: "unify",
    key: "pin_transitives",
    default_toml: "false",
    comment: "Pin transitive-only deps (workspace-hack replacement)",
  },
  FieldSpec {
    section: "unify",
    key: "transitive_host",
    default_toml: "\"root\"",
    comment: "Where to put pinned transitive dev-deps",
  },
  FieldSpec {
    section: "unify",
    key: "exclude",
    default_toml: "[]",
    comment: "Dependencies to skip unification (workspace-member cohorts exclude atomically)",
  },
  FieldSpec {
    section: "unify",
    key: "include",
    default_toml: "[]",
    comment: "Dependencies to force unification (workspace-member cohorts auto-include)",
  },
  FieldSpec {
    section: "unify",
    key: "msrv",
    default_toml: "true",
    comment: "Compute and write rust-version (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "msrv_source",
    default_toml: "\"max\"",
    comment: "How to compute MSRV: deps, workspace, max (default: max)",
  },
  FieldSpec {
    section: "unify",
    key: "enforce_msrv_inheritance",
    default_toml: "false",
    comment: "Ensure members inherit workspace rust-version",
  },
  FieldSpec {
    section: "unify",
    key: "prune_dead_features",
    default_toml: "true",
    comment: "Remove unused features (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "preserve_features",
    default_toml: "[]",
    comment: "Features to preserve from pruning (glob patterns)",
  },
  FieldSpec {
    section: "unify",
    key: "strict_version_compat",
    default_toml: "true",
    comment: "Treat version mismatches as errors (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "exact_pin_handling",
    default_toml: "\"warn\"",
    comment: "How to handle =x.y.z pins: skip, preserve, warn",
  },
  FieldSpec {
    section: "unify",
    key: "major_version_conflict",
    default_toml: "\"warn\"",
    comment: "How to handle major version conflicts: warn, bump",
  },
  FieldSpec {
    section: "unify",
    key: "detect_unused",
    default_toml: "true",
    comment: "Detect unused deps via resolved graph + rustc source diagnostics (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "compiler_diag_cache",
    default_toml: "true",
    comment: "Cache rustc source diagnostics for faster repeated unify runs (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "remove_unused",
    default_toml: "true",
    comment: "Auto-remove unused deps (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "detect_undeclared_features",
    default_toml: "true",
    comment: "Detect features borrowed via Cargo unification (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "fix_undeclared_features",
    default_toml: "true",
    comment: "Auto-fix borrowed features (default: true)",
  },
  FieldSpec {
    section: "unify",
    key: "skip_undeclared_patterns",
    default_toml: "[\"default\", \"std\", \"alloc\", \"*_backend\", \"*_impl\"]",
    comment: "Features to skip in undeclared detection (glob patterns)",
  },
  FieldSpec {
    section: "unify",
    key: "max_backups",
    default_toml: "3",
    comment: "Number of backup files to keep (default: 3)",
  },
  FieldSpec {
    section: "unify",
    key: "sort_dependencies",
    default_toml: "true",
    comment: "Sort deps alphabetically (default: true)",
  },
  // =========================================================================
  // [release] section - 11 fields
  // =========================================================================
  FieldSpec {
    section: "release",
    key: "tag_prefix",
    default_toml: "\"v\"",
    comment: "Prefix for tags",
  },
  FieldSpec {
    section: "release",
    key: "tag_format",
    default_toml: "\"{crate}-{prefix}{version}\"",
    comment: "Tag template: {crate}, {prefix}, {version}",
  },
  FieldSpec {
    section: "release",
    key: "require_clean",
    default_toml: "true",
    comment: "Require clean working directory",
  },
  FieldSpec {
    section: "release",
    key: "publish_delay",
    default_toml: "5",
    comment: "Seconds between crate publishes",
  },
  FieldSpec {
    section: "release",
    key: "create_github_release",
    default_toml: "false",
    comment: "Create GitHub releases via gh CLI",
  },
  FieldSpec {
    section: "release",
    key: "sign_tags",
    default_toml: "false",
    comment: "Sign git tags with GPG/SSH",
  },
  FieldSpec {
    section: "release",
    key: "changelog_path",
    default_toml: "\"CHANGELOG.md\"",
    comment: "Default changelog filename",
  },
  FieldSpec {
    section: "release",
    key: "changelog_relative_to",
    default_toml: "\"crate\"",
    comment: "Changelog path base: crate or workspace",
  },
  FieldSpec {
    section: "release",
    key: "skip_changelog_for",
    default_toml: "[]",
    comment: "Crates to skip changelog generation",
  },
  FieldSpec {
    section: "release",
    key: "require_changelog_entries",
    default_toml: "false",
    comment: "Error if no changelog entries for release",
  },
  FieldSpec {
    section: "release",
    key: "require_release_notes",
    default_toml: "true",
    comment: "Require release notes for target version before apply",
  },
  // =========================================================================
  // [change-detection] section - 4 fields
  // =========================================================================
  // Note: infrastructure has a complex default array. We provide a minimal
  // default here that users can expand. The full default from `cargo rail init`
  // includes more patterns, but this ensures the field exists.
  FieldSpec {
    section: "change-detection",
    key: "infrastructure",
    default_toml: "[\".github/**\", \"scripts/**\", \"justfile\", \"Justfile\", \"Makefile\", \"makefile\", \"GNUmakefile\", \"*.sh\", \"Taskfile.yml\", \"Taskfile.yaml\", \".pre-commit-config.yaml\", \"deny.toml\", \"cliff.toml\", \"release.toml\", \"release-plz.toml\"]",
    comment: "Files that trigger full workspace rebuild",
  },
  FieldSpec {
    section: "change-detection",
    key: "unknown_file_policy",
    default_toml: "\"strict\"",
    comment: "Unknown-file policy: docs, owned_build_test, workspace_infra, strict",
  },
  FieldSpec {
    section: "change-detection",
    key: "confidence_profile",
    default_toml: "\"balanced\"",
    comment: "Planner confidence profile: strict, balanced, fast",
  },
  FieldSpec {
    section: "change-detection",
    key: "bot_pr_confidence_profile",
    default_toml: "\"strict\"",
    comment: "Optional planner profile override for bot-authored PRs",
  },
  // =========================================================================
  // [run] section - 1 field
  // =========================================================================
  FieldSpec {
    section: "run",
    key: "default_profile",
    default_toml: "\"local\"",
    comment: "Default run profile (built-ins: local, ci, nightly)",
  },
  // Note: custom is a table {} not a simple value, so we don't auto-add it.
  // Users who want custom categories should add them manually.
];

/// Get fields for a specific section
pub fn fields_for_section(section: &str) -> impl Iterator<Item = &'static FieldSpec> {
  SYNCABLE_FIELDS.iter().filter(move |f| f.section == section)
}

/// Get all unique section names in order of first appearance
pub fn sections() -> Vec<&'static str> {
  let mut seen = Vec::new();
  for field in SYNCABLE_FIELDS {
    if !seen.contains(&field.section) {
      seen.push(field.section);
    }
  }
  seen
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_all_defaults_are_valid_toml() {
    for field in SYNCABLE_FIELDS {
      let toml_str = format!("{} = {}", field.key, field.default_toml);
      let result: Result<toml_edit::DocumentMut, _> = toml_str.parse();
      assert!(
        result.is_ok(),
        "Field {}.{} has invalid default TOML: {} (error: {:?})",
        field.section,
        field.key,
        field.default_toml,
        result.err()
      );
    }
  }

  #[test]
  fn test_sections_returns_unique_in_order() {
    let secs = sections();
    assert_eq!(secs[0], "unify");
    assert_eq!(secs[1], "release");
    assert_eq!(secs[2], "change-detection");
    assert_eq!(secs[3], "run");
    // No duplicates
    let mut unique = secs.clone();
    unique.dedup();
    assert_eq!(secs.len(), unique.len());
  }

  #[test]
  fn test_fields_for_section() {
    let unify_fields: Vec<_> = fields_for_section("unify").collect();
    assert_eq!(unify_fields.len(), 22); // 21 + compiler_diag_cache
    assert!(unify_fields.iter().all(|f| f.section == "unify"));

    let release_fields: Vec<_> = fields_for_section("release").collect();
    assert_eq!(release_fields.len(), 11);

    let change_detection_fields: Vec<_> = fields_for_section("change-detection").collect();
    assert_eq!(change_detection_fields.len(), 4);

    let run_fields: Vec<_> = fields_for_section("run").collect();
    assert_eq!(run_fields.len(), 1);
  }

  #[test]
  fn test_total_syncable_fields() {
    // This test ensures we don't accidentally remove fields
    // Update this count when adding new fields
    assert_eq!(
      SYNCABLE_FIELDS.len(),
      38, // 22 unify + 11 release + 4 change-detection + 1 run
      "Total syncable fields count changed - update this test if intentional"
    );
  }

  /// This test documents which config sections are syncable vs user-configured.
  ///
  /// SYNCABLE (auto-added by `config sync`):
  /// - [unify] - 22 fields: workspace-wide dependency unification settings
  /// - [release] - 11 fields: workspace-wide release settings
  /// - [change-detection] - 4 fields: infra patterns, conservative fallback, and confidence profiles
  /// - [run] - 1 field: default profile selector for `cargo rail run`
  ///
  /// NOT SYNCABLE (user must configure manually):
  /// - [crates.X.split] - Per-crate split configuration (remote, branch, mode, paths)
  /// - [crates.X.release] - Per-crate release overrides (publish)
  /// - [crates.X.changelog] - Per-crate changelog settings (path, skip)
  /// - [crates.X.sync] - Reserved for future use
  /// - [change-detection.custom] - User-defined custom categories (table, not simple field)
  ///
  /// The distinction is:
  /// - Syncable fields have sensible defaults that work for any workspace
  /// - Non-syncable fields require user knowledge of their specific setup
  #[test]
  fn test_schema_coverage_documented() {
    // Verify all three main sections are covered
    let sections: Vec<_> = sections().into_iter().collect();
    assert!(sections.contains(&"unify"), "unify section must be covered");
    assert!(sections.contains(&"release"), "release section must be covered");
    assert!(
      sections.contains(&"change-detection"),
      "change-detection section must be covered"
    );
    assert!(sections.contains(&"run"), "run section must be covered");

    // Verify key fields from each section exist
    let field_keys: Vec<_> = SYNCABLE_FIELDS.iter().map(|f| (f.section, f.key)).collect();

    // [unify] critical fields
    assert!(field_keys.contains(&("unify", "msrv")));
    assert!(field_keys.contains(&("unify", "msrv_source")));
    assert!(field_keys.contains(&("unify", "prune_dead_features")));
    assert!(field_keys.contains(&("unify", "preserve_features")));
    assert!(field_keys.contains(&("unify", "detect_unused")));
    assert!(field_keys.contains(&("unify", "compiler_diag_cache")));
    assert!(field_keys.contains(&("unify", "detect_undeclared_features")));
    assert!(field_keys.contains(&("unify", "fix_undeclared_features")));
    assert!(field_keys.contains(&("unify", "skip_undeclared_patterns")));

    // [release] critical fields
    assert!(field_keys.contains(&("release", "tag_format")));
    assert!(field_keys.contains(&("release", "changelog_path")));
    assert!(field_keys.contains(&("release", "require_release_notes")));

    // [change-detection] field
    assert!(field_keys.contains(&("change-detection", "infrastructure")));
    assert!(field_keys.contains(&("change-detection", "unknown_file_policy")));
    assert!(field_keys.contains(&("change-detection", "confidence_profile")));
    assert!(field_keys.contains(&("change-detection", "bot_pr_confidence_profile")));

    // [run] field
    assert!(field_keys.contains(&("run", "default_profile")));
  }
}
