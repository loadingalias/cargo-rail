//! High-level TOML document builders

use super::format::{TomlFormatter, TomlValue};
use crate::config::{ChangeDetectionConfig, ConfidenceProfile, ReleaseConfig, RunConfig, UnifyConfig};
use crate::error::RailResult;

/// Rail config builder
pub struct RailConfigBuilder {
  formatter: TomlFormatter,
  sections: Vec<String>,
}

impl Default for RailConfigBuilder {
  fn default() -> Self {
    Self {
      formatter: TomlFormatter::new(),
      sections: Vec::new(),
    }
  }
}

impl RailConfigBuilder {
  /// Create a new builder
  pub fn new() -> Self {
    Self::default()
  }

  /// Add header comments
  pub fn header(&mut self) -> &mut Self {
    self.sections.push(
      r#"# cargo-rail configuration
# Documentation: https://github.com/loadingalias/cargo-rail
"#
      .to_string(),
    );
    self
  }

  /// Add targets (workspace-wide, top-level field - must be FIRST after header)
  pub fn targets(&mut self, targets: &[String]) -> &mut Self {
    let mut content = String::new();
    if targets.is_empty() {
      content.push_str(
        "# targets = []  # optional: set for multi-target validation; `cargo rail init` can auto-detect from *.toml and .github/workflows\n",
      );
    } else {
      content.push_str(
        "# Targets for multi-platform validation (runs `cargo metadata --filter-platform <target>` per target)\n",
      );
      content.push_str(&format!("targets = {}\n", self.formatter.array_simple(targets)));
    }
    self.sections.push(content);
    self
  }

  /// Add unify section with logical groupings
  pub fn unify(&mut self, config: &UnifyConfig) -> &mut Self {
    let mut content = String::new();

    // === Basic ===
    content.push_str(&format!("include_paths = {}\n", config.include_paths));
    content.push_str(&format!("include_renamed = {}\n", config.include_renamed));

    // === Transitive pinning (workspace-hack replacement) ===
    content.push_str(&format!(
      "\npin_transitives = {}  # enable for hakari/workspace-hack users\n",
      config.pin_transitives
    ));
    match &config.transitive_host {
      crate::config::TransitiveFeatureHost::Root => {
        content.push_str("transitive_host = \"root\"  # only used if pin_transitives = true\n");
      }
      crate::config::TransitiveFeatureHost::Path(path) => {
        content.push_str(&format!("transitive_host = \"{}\"\n", path));
      }
    }

    // === Conflict handling ===
    content.push_str(&format!("\nstrict_version_compat = {}\n", config.strict_version_compat));
    let exact_pin_str = match config.exact_pin_handling {
      crate::config::ExactPinHandling::Skip => "skip",
      crate::config::ExactPinHandling::Preserve => "preserve",
      crate::config::ExactPinHandling::Warn => "warn",
    };
    content.push_str(&format!("exact_pin_handling = \"{}\"\n", exact_pin_str));
    let major_conflict_str = match config.major_version_conflict {
      crate::config::MajorVersionConflict::Warn => "warn",
      crate::config::MajorVersionConflict::Bump => "bump",
    };
    content.push_str(&format!(
      "major_version_conflict = \"{}\"  # warn = skip, bump = force highest\n",
      major_conflict_str
    ));

    // === MSRV ===
    content.push_str(&format!("\nmsrv = {}\n", config.msrv));
    let msrv_source_str = match config.msrv_source {
      crate::config::MsrvSource::Deps => "deps",
      crate::config::MsrvSource::Workspace => "workspace",
      crate::config::MsrvSource::Max => "max",
    };
    content.push_str(&format!(
      "msrv_source = \"{}\"  # deps | workspace | max\n",
      msrv_source_str
    ));
    content.push_str(&format!(
      "enforce_msrv_inheritance = {}  # Ensure members inherit workspace rust-version\n",
      config.enforce_msrv_inheritance
    ));

    // === Unused detection ===
    content.push_str(&format!("\ndetect_unused = {}\n", config.detect_unused));
    content.push_str(&format!(
      "compiler_diag_cache = {}  # reuse rustc diagnostics across runs\n",
      config.compiler_diag_cache
    ));
    content.push_str(&format!(
      "remove_unused = {}  # requires detect_unused = true\n",
      config.remove_unused
    ));

    // === Feature analysis ===
    content.push_str(&format!("\nprune_dead_features = {}\n", config.prune_dead_features));
    if config.preserve_features.is_empty() {
      content.push_str("preserve_features = []  # glob patterns to keep from pruning\n");
    } else {
      content.push_str(&format!(
        "preserve_features = {}\n",
        self.formatter.array_string(&config.preserve_features, None)
      ));
    }
    content.push_str(&format!(
      "detect_undeclared_features = {}\n",
      config.detect_undeclared_features
    ));
    content.push_str(&format!(
      "fix_undeclared_features = {}  # requires detect_undeclared_features = true\n",
      config.fix_undeclared_features
    ));
    if config.skip_undeclared_patterns.is_empty() {
      content.push_str("skip_undeclared_patterns = []\n");
    } else {
      content.push_str(&format!(
        "skip_undeclared_patterns = {}\n",
        self.formatter.array_string(&config.skip_undeclared_patterns, None)
      ));
    }

    // === Safety hatches ===
    if config.exclude.is_empty() {
      content.push_str("\nexclude = []  # excluding one workspace member excludes its whole member cohort\n");
    } else {
      content.push_str(&format!(
        "\nexclude = {}\n",
        self.formatter.array_string(&config.exclude, None)
      ));
    }
    if config.include.is_empty() {
      content.push_str("include = []  # workspace-member cohorts are auto-included\n");
    } else {
      content.push_str(&format!(
        "include = {}\n",
        self.formatter.array_string(&config.include, None)
      ));
    }
    content.push_str(&format!("max_backups = {}\n", config.max_backups));

    // === Formatting ===
    content.push_str(&format!(
      "sort_dependencies = {}  # false to preserve existing order\n",
      config.sort_dependencies
    ));

    self.sections.push(format!("[unify]\n{}", content));

    self
  }

  /// Add release section
  pub fn release(&mut self, config: &ReleaseConfig) -> &mut Self {
    let mut content = String::new();

    content.push_str(&format!("tag_prefix = \"{}\"\n", config.tag_prefix));
    content.push_str(&format!(
      "tag_format = \"{}\"  # e.g. my-crate-v1.0.0 (with tag_prefix = \"v\")\n",
      config.tag_format
    ));
    content.push_str(&format!("require_clean = {}\n", config.require_clean));
    content.push_str(&format!(
      "publish_delay = {}  # seconds between publishes\n",
      config.publish_delay
    ));
    content.push_str(&format!("create_github_release = {}\n", config.create_github_release));
    content.push_str(&format!("sign_tags = {}\n", config.sign_tags));
    content.push_str(&format!("changelog_path = \"{}\"\n", config.changelog_path));
    let changelog_relative_str = match config.changelog_relative_to {
      crate::config::ChangelogRelativeTo::Crate => "crate",
      crate::config::ChangelogRelativeTo::Workspace => "workspace",
    };
    content.push_str(&format!(
      "changelog_relative_to = \"{}\"  # crate = per-crate, workspace = single file\n",
      changelog_relative_str
    ));
    if config.skip_changelog_for.is_empty() {
      content.push_str("skip_changelog_for = []\n");
    } else {
      content.push_str(&format!(
        "skip_changelog_for = {}\n",
        self.formatter.array_string(&config.skip_changelog_for, None)
      ));
    }
    content.push_str(&format!(
      "require_changelog_entries = {}\n",
      config.require_changelog_entries
    ));
    content.push_str(&format!(
      "require_release_notes = {}  # fail if target version lacks release notes\n",
      config.require_release_notes
    ));

    self.sections.push(format!("\n[release]\n{}", content));
    self
  }

  /// Add change-detection section
  pub fn change_detection(&mut self, config: &ChangeDetectionConfig) -> &mut Self {
    let mut content = String::new();

    content.push_str("# Paths that trigger full workspace rebuild\n");
    content.push_str(&format!(
      "infrastructure = {}\n",
      self.formatter.array_string(&config.infrastructure, None)
    ));
    content.push_str(&format!(
      "conservative_unclassified_owner_fallback = {}\n",
      config.conservative_unclassified_owner_fallback
    ));
    let confidence_profile = match config.confidence_profile {
      ConfidenceProfile::Strict => "strict",
      ConfidenceProfile::Balanced => "balanced",
      ConfidenceProfile::Fast => "fast",
    };
    content.push_str(&format!("confidence_profile = \"{}\"\n", confidence_profile));
    let bot_profile = match config.bot_pr_confidence_profile.unwrap_or(ConfidenceProfile::Strict) {
      ConfidenceProfile::Strict => "strict",
      ConfidenceProfile::Balanced => "balanced",
      ConfidenceProfile::Fast => "fast",
    };
    content.push_str(&format!(
      "bot_pr_confidence_profile = \"{}\"  # optional bot PR override\n",
      bot_profile
    ));

    if !config.custom.is_empty() {
      content.push_str("\n[change-detection.custom]\n");
      let mut categories: Vec<_> = config.custom.iter().collect();
      categories.sort_by(|(a, _), (b, _)| a.cmp(b));

      for (category, patterns) in categories {
        content.push_str(&format!(
          "{} = {}\n",
          category,
          self.formatter.array_string(patterns, None)
        ));
      }
    }

    self.sections.push(format!("\n[change-detection]\n{}", content));
    self
  }

  /// Add run profile section.
  pub fn run(&mut self, config: &RunConfig) -> &mut Self {
    let mut content = String::new();

    content.push_str(&format!(
      "default_profile = \"{}\"\n",
      config.default_profile.as_deref().unwrap_or("local")
    ));

    if !config.workflow.is_empty() {
      content.push_str("\n[run.workflow]\n");
      let mut workflows: Vec<_> = config.workflow.iter().collect();
      workflows.sort_by(|(a, _), (b, _)| a.cmp(b));
      for (workflow, profile) in workflows {
        content.push_str(&format!("{} = \"{}\"\n", workflow, profile));
      }
    }

    if !config.profiles.is_empty() {
      let mut profiles: Vec<_> = config.profiles.iter().collect();
      profiles.sort_by(|(a, _), (b, _)| a.cmp(b));
      for (profile_name, profile) in profiles {
        content.push_str(&format!("\n[run.profile.{}]\n", profile_name));
        content.push_str(&format!(
          "surfaces = {}\n",
          self.formatter.array_string(&profile.surfaces, None)
        ));
        if !profile.run_args.is_empty() {
          content.push_str(&format!(
            "run_args = {}\n",
            self.formatter.array_string(&profile.run_args, None)
          ));
        }
        if let Some(since) = &profile.since {
          content.push_str(&format!("since = \"{}\"\n", since));
        }
        if let Some(merge_base) = profile.merge_base {
          content.push_str(&format!("merge_base = {}\n", merge_base));
        }
      }
    } else {
      content.push_str("\n# Optional workflow -> profile mapping:\n");
      content.push_str("# [run.workflow]\n");
      content.push_str("# commit = \"ci\"\n");
      content.push_str("# nightly = \"nightly\"\n");
      content.push_str("\n# Optional user-defined run profiles:\n");
      content.push_str("# [run.profile.ci]\n");
      content.push_str("# surfaces = [\"build\", \"test\"]\n");
      content.push_str("# merge_base = true\n");
      content.push_str("\n# [run.profile.docs_custom]\n");
      content.push_str("# surfaces = [\"docs\"]\n");
      content.push_str("# run_args = [\"--workspace\", \"{cargo_args}\"]\n");
      content.push_str("# since = \"{base_ref}\"\n");
    }

    self.sections.push(format!("\n[run]\n{}", content));
    self
  }

  /// Add splits template
  pub fn splits_template(&mut self) -> &mut Self {
    let content = r#"
# Per-crate configuration: run 'cargo rail split init <crate>' to generate
# [crates.my-crate.split]
# remote = "git@github.com:org/my-crate.git"
# branch = "main"
# mode = "single"
# paths = [{ crate = "crates/my-crate" }]
"#;
    self.sections.push(content.to_string());
    self
  }

  /// Build final TOML string
  pub fn build(&self) -> RailResult<String> {
    Ok(self.sections.join("\n"))
  }
}

/// Cargo.toml workspace dependencies builder
pub struct WorkspaceDepsBuilder {
  formatter: TomlFormatter,
  deps: Vec<(String, String, Option<String>)>, // Name, Value (formatted), Optional comment
  /// Whether to sort dependencies alphabetically (default: true)
  pub sort_dependencies: bool,
}

impl Default for WorkspaceDepsBuilder {
  fn default() -> Self {
    Self {
      formatter: TomlFormatter::new(),
      deps: Vec::new(),
      sort_dependencies: true,
    }
  }
}

impl WorkspaceDepsBuilder {
  /// Create a new builder
  pub fn new() -> Self {
    Self::default()
  }

  /// Set whether to sort dependencies alphabetically (default: true)
  pub fn with_dependency_sort(mut self, sort: bool) -> Self {
    self.sort_dependencies = sort;
    self.formatter.sort_dependencies = sort;
    self
  }

  /// Add dependency
  pub fn add(&mut self, name: &str, value: &str) -> &mut Self {
    self.deps.push((name.to_string(), value.to_string(), None));
    self
  }

  /// Add dependency with comment
  pub fn add_with_comment(&mut self, name: &str, value: &str, comment: &str) -> &mut Self {
    self
      .deps
      .push((name.to_string(), value.to_string(), Some(comment.to_string())));
    self
  }

  /// Add dependency with inline table
  pub fn add_table(&mut self, name: &str, pairs: &[(String, TomlValue)]) -> &mut Self {
    let value = self.formatter.inline_table(pairs);
    self.deps.push((name.to_string(), value, None));
    self
  }

  /// Add dependency with inline table and comment
  pub fn add_table_with_comment(&mut self, name: &str, pairs: &[(String, TomlValue)], comment: &str) -> &mut Self {
    let value = self.formatter.inline_table(pairs);
    self.deps.push((name.to_string(), value, Some(comment.to_string())));
    self
  }

  /// Build [workspace.dependencies] section
  pub fn build(&self) -> RailResult<String> {
    let mut content = String::from("\n[workspace.dependencies]\n");

    // Use indices + sorting to avoid cloning the entire Vec
    let mut indices: Vec<usize> = (0..self.deps.len()).collect();
    if self.sort_dependencies {
      indices.sort_by(|&a, &b| self.deps[a].0.cmp(&self.deps[b].0));
    }

    for idx in indices {
      let (name, value, comment) = &self.deps[idx];
      if let Some(c) = comment {
        content.push_str(&format!("{} = {}  # {}\n", name, value, c));
      } else {
        content.push_str(&format!("{} = {}\n", name, value));
      }
    }

    Ok(content)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::RailConfig;

  #[test]
  fn test_rail_config_builder() {
    let mut builder = RailConfigBuilder::new();
    let config = RailConfig {
      targets: vec!["x86_64-unknown-linux-gnu".to_string()],
      unify: UnifyConfig::default(),
      release: ReleaseConfig::default(),
      change_detection: crate::config::ChangeDetectionConfig::default(),
      run: crate::config::RunConfig::default(),
      crates: Default::default(),
    };

    let output = builder
      .header()
      .targets(&config.targets)
      .unify(&config.unify)
      .release(&config.release)
      .change_detection(&config.change_detection)
      .run(&config.run)
      .splits_template()
      .build()
      .unwrap();

    assert!(output.contains("targets"));
    assert!(output.contains("[unify]"));
    assert!(output.contains("[release]"));
    assert!(output.contains("[change-detection]"));
    assert!(output.contains("[run]"));
    assert!(output.contains("[crates.my-crate.split]"));
  }

  #[test]
  fn test_change_detection_custom_categories_are_sorted() {
    use rustc_hash::FxHashMap;

    let mut builder = RailConfigBuilder::new();
    let mut custom = FxHashMap::default();
    custom.insert("zeta".to_string(), vec!["z/**".to_string()]);
    custom.insert("alpha".to_string(), vec!["a/**".to_string()]);
    let config = ChangeDetectionConfig {
      infrastructure: vec![".github/**".to_string()],
      custom,
      conservative_unclassified_owner_fallback: true,
      confidence_profile: ConfidenceProfile::Balanced,
      bot_pr_confidence_profile: None,
    };

    let output = builder.header().change_detection(&config).build().unwrap();
    let alpha_idx = output.find("alpha = [\"a/**\"]").unwrap();
    let zeta_idx = output.find("zeta = [\"z/**\"]").unwrap();
    assert!(alpha_idx < zeta_idx);
  }

  #[test]
  fn test_workspace_deps_builder() {
    let mut builder = WorkspaceDepsBuilder::new();
    builder.add("anyhow", "\"1.0\"");
    builder.add_table(
      "serde",
      &[
        ("version".to_string(), TomlValue::String("1.0".to_string())),
        ("features".to_string(), TomlValue::Array(vec!["derive".to_string()])),
      ],
    );

    let output = builder.build().unwrap();
    assert!(output.contains("anyhow = \"1.0\""));
    assert!(output.contains("serde = { version = \"1.0\", features = [\"derive\"] }"));
  }
}
