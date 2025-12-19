//! High-level TOML document builders

use super::format::{TomlFormatter, TomlValue};
use crate::config::{ChangeDetectionConfig, ReleaseConfig, UnifyConfig};
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
    content.push_str(&format!(
      "enforce_msrv_inheritance = {}  # set rust-version = {{ workspace = true }} for members\n",
      config.enforce_msrv_inheritance
    ));
    let msrv_source_str = match config.msrv_source {
      crate::config::MsrvSource::Deps => "deps",
      crate::config::MsrvSource::Workspace => "workspace",
      crate::config::MsrvSource::Max => "max",
    };
    content.push_str(&format!(
      "msrv_source = \"{}\"  # deps | workspace | max\n",
      msrv_source_str
    ));

    // === Unused detection ===
    content.push_str(&format!("\ndetect_unused = {}\n", config.detect_unused));
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
      content.push_str("\nexclude = []\n");
    } else {
      content.push_str(&format!(
        "\nexclude = {}\n",
        self.formatter.array_string(&config.exclude, None)
      ));
    }
    if config.include.is_empty() {
      content.push_str("include = []\n");
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

    if !config.custom.is_empty() {
      content.push_str("\n[change-detection.custom]\n");
      for (category, patterns) in &config.custom {
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

    let deps_to_write = if self.sort_dependencies {
      let mut sorted = self.deps.clone();
      sorted.sort_by(|a, b| a.0.cmp(&b.0));
      sorted
    } else {
      self.deps.clone()
    };

    for (name, value, comment) in deps_to_write {
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
      crates: Default::default(),
    };

    let output = builder
      .header()
      .targets(&config.targets)
      .unify(&config.unify)
      .release(&config.release)
      .change_detection(&config.change_detection)
      .splits_template()
      .build()
      .unwrap();

    assert!(output.contains("targets"));
    assert!(output.contains("[unify]"));
    assert!(output.contains("[release]"));
    assert!(output.contains("[change-detection]"));
    assert!(output.contains("[crates.my-crate.split]"));
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
