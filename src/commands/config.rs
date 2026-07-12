//! `cargo rail config` - Configuration management commands

use crate::commands::common::TextJsonOutputFormat;
use crate::config::{ConfigLoadResult, RailConfig, schema};
use crate::error::{RailError, RailResult};
use crate::toml::TomlEditor;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

fn print_config_json<T: Serialize>(mode: &str, result: &str, exit_code: i32, payload: &T) -> RailResult<()> {
  let payload_value = serde_json::to_value(payload).map_err(|e| RailError::message(e.to_string()))?;
  let output = crate::output::machine_json_envelope("config", mode, result, exit_code, payload_value);
  println!(
    "{}",
    serde_json::to_string_pretty(&output).map_err(|e| RailError::message(e.to_string()))?
  );
  Ok(())
}

/// Validation result for JSON output
#[derive(Serialize)]
struct ValidationResult {
  command: &'static str,
  action: &'static str,
  valid: bool,
  config_path: Option<String>,
  errors: Vec<ValidationIssue>,
  warnings: Vec<ValidationIssue>,
}

/// A single validation issue
#[derive(Serialize, Clone)]
struct ValidationIssue {
  section: String,
  message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  line: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  column: Option<usize>,
}

impl ValidationIssue {
  fn new(section: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      section: section.into(),
      message: message.into(),
      line: None,
      column: None,
    }
  }

  fn with_location(mut self, line: usize, column: usize) -> Self {
    self.line = Some(line);
    self.column = Some(column);
    self
  }
}

/// Strictness mode for validation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictnessMode {
  /// Explicit --strict flag
  Strict,
  /// Explicit --no-strict flag
  NoStrict,
  /// Auto-detect based on CI environment
  Auto,
}

impl StrictnessMode {
  /// Determine if we should be strict based on mode and environment
  pub fn is_strict(&self) -> bool {
    match self {
      StrictnessMode::Strict => true,
      StrictnessMode::NoStrict => false,
      StrictnessMode::Auto => is_ci_environment(),
    }
  }
}

/// Check if running in a CI environment
fn is_ci_environment() -> bool {
  std::env::var("CI").is_ok()
    || std::env::var("GITHUB_ACTIONS").is_ok()
    || std::env::var("GITLAB_CI").is_ok()
    || std::env::var("CIRCLECI").is_ok()
}

/// Known top-level keys in rail.toml
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
  "targets",
  "unify",
  "release",
  "change-detection",
  "run",
  "crates",
  "workspace",
  "toolchain",
];

/// Known keys in [unify] section
const KNOWN_UNIFY_KEYS: &[&str] = &[
  "include_paths",
  "include_renamed",
  "pin_transitives",
  "transitive_host",
  "exclude",
  "include",
  "msrv",
  "msrv_source",
  "enforce_msrv_inheritance",
  "prune_dead_features",
  "consumer_scope",
  "preserve_features",
  "strict_version_compat",
  "exact_pin_handling",
  "major_version_conflict",
  "detect_unused",
  "compiler_diag_cache",
  "remove_unused",
  "detect_undeclared_features",
  "fix_undeclared_features",
  "skip_undeclared_patterns",
  "max_backups",
  "sort_dependencies",
];

/// Known keys in [release] section
const KNOWN_RELEASE_KEYS: &[&str] = &[
  "tag_prefix",
  "tag_format",
  "require_clean",
  "publish_delay",
  "create_github_release",
  "forge",
  "push",
  "sign_tags",
  "changelog",
  "require_changelog_entries",
  "require_release_notes",
  "release_notes_dir",
  "change_dir",
  "pre_1_breaking_bump",
  "unconventional_commits",
  "semver_check",
  "require_change_files",
  "version_groups",
];

/// Known keys in [release.changelog] section
const KNOWN_RELEASE_CHANGELOG_KEYS: &[&str] = &[
  "path",
  "relative_to",
  "entry_format",
  "emoji",
  "group_order",
  "fallback",
  "groups",
  "filters",
  "commit_url",
  "pr_url",
];

/// Known keys in [release.changelog.filters] section
const KNOWN_RELEASE_CHANGELOG_FILTER_KEYS: &[&str] = &["skip_types", "skip_scopes", "include_paths", "exclude_paths"];

/// Known keys in [change-detection] section
const KNOWN_CHANGE_DETECTION_KEYS: &[&str] = &[
  "infrastructure",
  "custom",
  "unknown_file_policy",
  "confidence_profile",
  "bot_pr_confidence_profile",
];

/// Known keys in [run] section
const KNOWN_RUN_KEYS: &[&str] = &["default_profile", "profile", "workflow"];
/// Known keys in [run.profile.<name>] section
const KNOWN_RUN_PROFILE_KEYS: &[&str] = &["surfaces", "run_args", "since", "merge_base"];

// Config Locate

/// Result of config locate for JSON output
#[derive(Serialize)]
struct LocateResult {
  command: &'static str,
  action: &'static str,
  found: bool,
  path: Option<String>,
  search_paths: Vec<String>,
}

/// Print the path to the active configuration file
///
/// This is the equivalent of `cargo locate-project` for rail.toml.
/// Searches in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
pub fn run_config_locate(
  workspace_root: &Path,
  config_override: Option<&Path>,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  if json {
    crate::output::set_json_mode(true);
  }

  // If config path was explicitly provided, use it
  if let Some(explicit_path) = config_override {
    let path = if explicit_path.is_absolute() {
      explicit_path.to_path_buf()
    } else {
      workspace_root.join(explicit_path)
    };

    if path.exists() {
      if json {
        let result = LocateResult {
          command: "config",
          action: "locate",
          found: true,
          path: Some(path.display().to_string()),
          search_paths: vec![],
        };
        print_config_json("locate", "success", 0, &result)?;
      } else {
        println!("{}", path.display());
      }
      return Ok(());
    } else {
      return Err(RailError::message(format!(
        "specified config file not found: {}",
        path.display()
      )));
    }
  }

  // Search standard locations
  let search_paths = [
    workspace_root.join("rail.toml"),
    workspace_root.join(".rail.toml"),
    workspace_root.join(".cargo").join("rail.toml"),
    workspace_root.join(".config").join("rail.toml"),
  ];

  let config_path = RailConfig::find_config_path(workspace_root);

  if json {
    let result = LocateResult {
      command: "config",
      action: "locate",
      found: config_path.is_some(),
      path: config_path.as_ref().map(|p| p.display().to_string()),
      search_paths: search_paths.iter().map(|p| p.display().to_string()).collect(),
    };
    print_config_json(
      "locate",
      if config_path.is_some() { "success" } else { "not_found" },
      0,
      &result,
    )?;
  } else if let Some(path) = &config_path {
    println!("{}", path.display());
  } else {
    println!("no config file found");
    println!();
    println!("searched:");
    for p in &search_paths {
      println!("  {}", p.display());
    }
    println!();
    println!("hint: run 'cargo rail init' to create one");
    return Err(RailError::ExitWithCode { code: 1 });
  }

  Ok(())
}

// Config Print

/// Print the effective configuration with defaults merged
///
/// Shows what cargo-rail will actually use: user settings plus defaults
/// for any fields not explicitly set.
pub fn run_config_print(
  workspace_root: &Path,
  config_override: Option<&Path>,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  if json {
    crate::output::set_json_mode(true);
  }

  // Load config from explicit path or search
  let (config, config_path) = load_config_with_path(workspace_root, config_override)?;

  if json {
    // JSON output: serialize the config struct
    #[derive(Serialize)]
    struct PrintResult {
      command: &'static str,
      action: &'static str,
      config_path: String,
      config: RailConfig,
    }

    let result = PrintResult {
      command: "config",
      action: "print",
      config_path: config_path.display().to_string(),
      config,
    };
    print_config_json("print", "success", 0, &result)?;
  } else {
    // TOML output: serialize to TOML with a header comment
    println!("# Effective configuration (loaded from {})", config_path.display());
    println!("# This shows all settings including defaults for unset fields.");
    println!();

    let toml_str = toml_edit::ser::to_string_pretty(&config)
      .map_err(|e| RailError::message(format!("failed to serialize config: {}", e)))?;
    print!("{}", toml_str);
  }

  Ok(())
}

/// Load config from explicit path or search, returning both config and path
fn load_config_with_path(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<(RailConfig, PathBuf)> {
  if let Some(explicit_path) = config_override {
    let path = if explicit_path.is_absolute() {
      explicit_path.to_path_buf()
    } else {
      workspace_root.join(explicit_path)
    };

    if !path.exists() {
      return Err(RailError::message(format!(
        "specified config file not found: {}",
        path.display()
      )));
    }

    let content = std::fs::read_to_string(&path)
      .map_err(|e| RailError::message(format!("failed to read {}: {}", path.display(), e)))?;

    let config: RailConfig = toml_edit::de::from_str(&content)
      .map_err(|e| RailError::message(format!("failed to parse {}: {}", path.display(), e)))?;

    return Ok((config, path));
  }

  // Search standard locations
  let config_path = RailConfig::find_config_path(workspace_root).ok_or_else(|| {
    RailError::with_help(
      "no rail.toml found".to_string(),
      "run 'cargo rail init' first to create a configuration file".to_string(),
    )
  })?;

  match RailConfig::try_load(workspace_root) {
    ConfigLoadResult::Loaded(config) => Ok((*config, config_path)),
    ConfigLoadResult::ParseError { path, message } => Err(RailError::message(format!(
      "failed to parse {}: {}",
      path.display(),
      message
    ))),
    ConfigLoadResult::NotFound => Err(RailError::message("config file not found".to_string())),
  }
}

// Config Validate

/// Validate configuration file standalone (without WorkspaceContext)
///
/// This function can diagnose parse errors and unknown keys even when
/// the config file is broken.
pub fn run_config_validate_standalone(
  workspace_root: &Path,
  config_override: Option<&Path>,
  format: TextJsonOutputFormat,
  strictness: StrictnessMode,
) -> RailResult<()> {
  let json = format.is_json();
  let strict = strictness.is_strict();

  if json {
    crate::output::set_json_mode(true);
  }

  let mut errors: Vec<ValidationIssue> = Vec::new();
  let mut warnings: Vec<ValidationIssue> = Vec::new();

  // Find config file
  let config_path = match resolve_config_path(workspace_root, config_override) {
    Ok(path) => path,
    Err(err) => {
      let is_default_lookup_miss = config_override.is_none();
      if json {
        let result = ValidationResult {
          command: "config",
          action: "validate",
          valid: false,
          config_path: None,
          errors: vec![ValidationIssue::new(
            "config",
            if is_default_lookup_miss {
              "no configuration file found".to_string()
            } else {
              err.to_string()
            },
          )],
          warnings: vec![],
        };
        print_config_json("validate", "failed", 2, &result)?;
        return Err(RailError::ExitWithCode { code: 2 });
      }
      if is_default_lookup_miss {
        println!("no configuration file found");
        println!("\nhelp: run 'cargo rail init' to create one");
        return Err(RailError::message("no configuration file found"));
      }
      return Err(err);
    }
  };

  // Read raw content for unknown key detection
  let content = std::fs::read_to_string(&config_path)
    .map_err(|e| RailError::message(format!("failed to read {}: {}", config_path.display(), e)))?;

  // Check for parse errors with line/column info
  let raw_doc: Result<toml_edit::DocumentMut, _> = content.parse();
  if let Err(parse_err) = &raw_doc {
    let err_str = parse_err.to_string();
    // Try to extract line/column from toml_edit error
    let issue = if let Some((line, col)) = extract_toml_error_location(&err_str) {
      ValidationIssue::new("syntax", format!("TOML parse error: {}", err_str)).with_location(line, col)
    } else {
      ValidationIssue::new("syntax", format!("TOML parse error: {}", err_str))
    };
    errors.push(issue);
  }

  // Check for unknown keys (only if parsing succeeded)
  if let Ok(doc) = &raw_doc {
    check_unknown_keys(doc, &mut warnings);
  }

  // Try to load and validate semantically
  match parse_config_from_path(&config_path) {
    Ok(config) => {
      // Validate change detection config
      if let Err(e) = config.change_detection.validate() {
        errors.push(ValidationIssue::new("change_detection", e.to_string()));
      }
      if let Err(e) = config.run.validate() {
        errors.push(ValidationIssue::new("run", e.to_string()));
      }
      if let Err(e) = config.release.changelog.filters.validate("release.changelog.filters") {
        errors.push(ValidationIssue::new("release.changelog.filters", e.to_string()));
      }

      // Validate per-crate split config
      for (crate_name, crate_config) in &config.crates {
        if let Some(changelog_cfg) = &crate_config.changelog
          && let Some(filters) = &changelog_cfg.filters
          && let Err(e) = filters.validate(&format!("crates.{}.changelog.filters", crate_name))
        {
          errors.push(ValidationIssue::new(
            format!("crates.{}.changelog.filters", crate_name),
            e.to_string(),
          ));
        }
        if let Some(split_cfg) = &crate_config.split {
          if split_cfg.remote.is_empty() {
            errors.push(ValidationIssue::new(
              format!("crates.{}.split", crate_name),
              "missing required field: remote",
            ));
          }
          if split_cfg.branch.is_empty() {
            warnings.push(ValidationIssue::new(
              format!("crates.{}.split", crate_name),
              "branch is empty, will use default",
            ));
          }
        }
      }

      // Check targets are valid
      for target in &config.targets {
        if !target.contains('-') {
          warnings.push(ValidationIssue::new(
            "targets",
            format!("'{}' doesn't look like a valid target triple", target),
          ));
        }
      }
    }
    Err(err) => {
      // Only add if we didn't already catch it in raw parsing
      if errors.is_empty() {
        errors.push(ValidationIssue::new("config", format!("failed to load: {}", err)));
      }
    }
  }

  // In strict mode, warnings become errors
  let (final_errors, final_warnings) = if strict {
    let mut all_errors = errors;
    all_errors.extend(warnings.iter().cloned());
    (all_errors, vec![])
  } else {
    (errors, warnings)
  };

  let valid = final_errors.is_empty();

  // Output
  if json {
    let result = ValidationResult {
      command: "config",
      action: "validate",
      valid,
      config_path: Some(config_path.display().to_string()),
      errors: final_errors,
      warnings: final_warnings,
    };
    print_config_json(
      "validate",
      if valid { "success" } else { "failed" },
      if valid { 0 } else { 2 },
      &result,
    )?;
  } else {
    println!("config: {}", config_path.display());
    if strict && is_ci_environment() {
      println!("mode: strict (CI detected)");
    } else if strict {
      println!("mode: strict");
    }
    println!();

    if !final_errors.is_empty() {
      println!("errors:");
      for e in &final_errors {
        if let (Some(line), Some(col)) = (e.line, e.column) {
          println!("  [{}:{}:{}] {}", e.section, line, col, e.message);
        } else {
          println!("  [{}] {}", e.section, e.message);
        }
      }
      println!();
    }

    if !final_warnings.is_empty() {
      println!("warnings:");
      for w in &final_warnings {
        println!("  [{}] {}", w.section, w.message);
      }
      println!();
    }

    if valid {
      println!("configuration is valid");
    } else {
      println!("configuration has {} error(s)", final_errors.len());
    }
  }

  if valid {
    Ok(())
  } else if json {
    Err(RailError::ExitWithCode { code: 2 })
  } else {
    Err(RailError::message("configuration validation failed"))
  }
}

/// Extract line/column from toml_edit error message if present
fn extract_toml_error_location(err: &str) -> Option<(usize, usize)> {
  // toml_edit errors often contain "at line X column Y"
  if let Some(at_pos) = err.find("at line ") {
    let rest = &err[at_pos + 8..];
    let parts: Vec<&str> = rest.split_whitespace().take(3).collect();
    if parts.len() >= 3
      && parts[1] == "column"
      && let (Ok(line), Ok(col)) = (parts[0].parse::<usize>(), parts[2].parse::<usize>())
    {
      return Some((line, col));
    }
  }
  None
}

/// Check for unknown keys in the TOML document
fn check_unknown_keys(doc: &toml_edit::DocumentMut, warnings: &mut Vec<ValidationIssue>) {
  let known_top: HashSet<&str> = KNOWN_TOP_LEVEL_KEYS.iter().copied().collect();

  for (key, item) in doc.iter() {
    if !known_top.contains(key) {
      warnings.push(ValidationIssue::new(
        "config",
        format!("unknown top-level key '{}'", key),
      ));
      continue;
    }

    // Check nested keys
    if let Some(table) = item.as_table() {
      let known_keys: Option<HashSet<&str>> = match key {
        "unify" => Some(KNOWN_UNIFY_KEYS.iter().copied().collect()),
        "release" => Some(KNOWN_RELEASE_KEYS.iter().copied().collect()),
        "change-detection" => Some(KNOWN_CHANGE_DETECTION_KEYS.iter().copied().collect()),
        "run" => Some(KNOWN_RUN_KEYS.iter().copied().collect()),
        _ => None, // crates, workspace, toolchain have dynamic keys
      };

      if let Some(known) = known_keys {
        for (nested_key, _) in table.iter() {
          if !known.contains(nested_key) {
            warnings.push(ValidationIssue::new(
              key,
              format!("unknown key '{}' in [{}] section", nested_key, key),
            ));
          }
        }

        if key == "run" {
          check_run_nested_keys(table, warnings);
        }
        if key == "release" {
          check_release_nested_keys(table, warnings);
        }
      }
    }
  }
}

fn check_release_nested_keys(table: &toml_edit::Table, warnings: &mut Vec<ValidationIssue>) {
  if let Some(changelog_item) = table.get("changelog")
    && let Some(changelog_table) = changelog_item.as_table()
  {
    let known: HashSet<&str> = KNOWN_RELEASE_CHANGELOG_KEYS.iter().copied().collect();
    for (key, value) in changelog_table.iter() {
      if !known.contains(key) {
        warnings.push(ValidationIssue::new(
          "release",
          format!("unknown key '{}' in [release.changelog] section", key),
        ));
      }

      if key == "filters"
        && let Some(filters) = value.as_table()
      {
        let known_filters: HashSet<&str> = KNOWN_RELEASE_CHANGELOG_FILTER_KEYS.iter().copied().collect();
        for (filter_key, _) in filters.iter() {
          if !known_filters.contains(filter_key) {
            warnings.push(ValidationIssue::new(
              "release",
              format!("unknown key '{}' in [release.changelog.filters] section", filter_key),
            ));
          }
        }
      }
    }
  }
}

fn check_run_nested_keys(table: &toml_edit::Table, warnings: &mut Vec<ValidationIssue>) {
  if let Some(profile_item) = table.get("profile")
    && let Some(profile_table) = profile_item.as_table()
  {
    let known: HashSet<&str> = KNOWN_RUN_PROFILE_KEYS.iter().copied().collect();
    for (profile_name, profile_value) in profile_table.iter() {
      let Some(profile_cfg) = profile_value.as_table() else {
        warnings.push(ValidationIssue::new(
          "run",
          format!("run.profile.{} must be a table", profile_name),
        ));
        continue;
      };

      for (profile_key, _) in profile_cfg.iter() {
        if !known.contains(profile_key) {
          warnings.push(ValidationIssue::new(
            "run",
            format!(
              "unknown key '{}' in [run.profile.{}] section",
              profile_key, profile_name
            ),
          ));
        }
      }
    }
  }
}

// Config Sync

/// Result of target sync operation
#[derive(Debug, Clone, Serialize)]
pub struct TargetSyncResult {
  /// Targets added during sync
  pub added: Vec<String>,
  /// Targets removed during sync
  pub removed: Vec<String>,
  /// Total targets after sync
  pub total: usize,
}

impl TargetSyncResult {
  /// Check if any targets were added or removed
  pub fn has_changes(&self) -> bool {
    !self.added.is_empty() || !self.removed.is_empty()
  }
}

/// Result of field sync operation
#[derive(Debug, Clone, Serialize)]
pub struct FieldChange {
  /// TOML path of the field (e.g., "unify.msrv_source")
  pub path: String,
  /// Default value that was inserted
  pub value: String,
}

/// Complete sync result for JSON output
#[derive(Serialize)]
struct ConfigSyncResult {
  command: &'static str,
  action: &'static str,
  config_path: String,
  fields_added: Vec<FieldChange>,
  targets: Option<TargetSyncResult>,
  has_changes: bool,
}

/// Sync configuration: add missing fields and update targets
///
/// This command ensures rail.toml has all known configuration fields
/// without overwriting existing user values. It also syncs target triples
/// from workspace configuration files (rust-toolchain.toml, .cargo/config.toml, etc.)
///
/// Exit codes:
/// - 0: Config is up to date (no changes needed or changes applied)
/// - 1: Changes detected (--check mode only)
/// - 2: Error
pub fn run_config_sync(
  workspace_root: &Path,
  config_override: Option<&Path>,
  check: bool,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  if json {
    crate::output::set_json_mode(true);
  }

  // Find existing config
  let config_path = resolve_config_path(workspace_root, config_override)?;

  let mut editor = TomlEditor::open(&config_path)?;
  let mut fields_added: Vec<FieldChange> = Vec::new();

  // Add missing fields from schema
  for field in schema::SYNCABLE_FIELDS {
    let path = format!("{}.{}", field.section, field.key);

    // Ensure section exists
    editor.ensure_section(field.section);

    // Check if field exists
    if !editor.contains_path(&path) {
      editor.set_raw_with_comment(&path, field.default_toml, Some(field.comment))?;
      fields_added.push(FieldChange {
        path: path.clone(),
        value: field.default_toml.to_string(),
      });
    }
  }

  // Sync targets from workspace
  let targets_result = sync_targets(&mut editor, workspace_root)?;

  // Check for changes
  let has_changes = !fields_added.is_empty() || targets_result.as_ref().is_some_and(|t| t.has_changes());

  if check {
    // Preview mode
    if json {
      let result = ConfigSyncResult {
        command: "config",
        action: "sync",
        config_path: config_path.display().to_string(),
        fields_added,
        targets: targets_result,
        has_changes,
      };
      print_config_json(
        "sync",
        if has_changes { "pending_changes" } else { "success" },
        if has_changes { 1 } else { 0 },
        &result,
      )?;
    } else {
      print_sync_preview(&config_path, &fields_added, &targets_result, has_changes);
    }

    if has_changes {
      // Exit with code 1 to indicate changes are needed
      return Err(RailError::CheckHasPendingChanges);
    }
  } else {
    // Apply mode
    if !has_changes {
      if json {
        let result = ConfigSyncResult {
          command: "config",
          action: "sync",
          config_path: config_path.display().to_string(),
          fields_added: vec![],
          targets: targets_result,
          has_changes: false,
        };
        print_config_json("sync", "success", 0, &result)?;
      } else {
        println!("config: {} (up to date)", config_path.display());
      }
      return Ok(());
    }

    editor.write()?;

    if json {
      let result = ConfigSyncResult {
        command: "config",
        action: "sync",
        config_path: config_path.display().to_string(),
        fields_added,
        targets: targets_result,
        has_changes: true,
      };
      print_config_json("sync", "applied", 0, &result)?;
    } else {
      print_sync_applied(&config_path, &fields_added, &targets_result);
    }
  }

  Ok(())
}

fn resolve_config_path(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<PathBuf> {
  if let Some(explicit_path) = config_override {
    let path = if explicit_path.is_absolute() {
      explicit_path.to_path_buf()
    } else {
      workspace_root.join(explicit_path)
    };
    if path.exists() {
      return Ok(path);
    }
    return Err(RailError::message(format!(
      "specified config file not found: {}",
      path.display()
    )));
  }

  RailConfig::find_config_path(workspace_root).ok_or_else(|| {
    RailError::with_help(
      "no rail.toml found".to_string(),
      "run 'cargo rail init' first to create a configuration file".to_string(),
    )
  })
}

fn parse_config_from_path(config_path: &Path) -> RailResult<RailConfig> {
  let content = std::fs::read_to_string(config_path)
    .map_err(|e| RailError::message(format!("failed to read {}: {}", config_path.display(), e)))?;
  toml_edit::de::from_str::<RailConfig>(&content)
    .map_err(|e| RailError::message(format!("failed to parse {}: {}", config_path.display(), e)))
}

/// Sync targets from workspace into config
///
/// This performs a **union merge**: existing user-configured targets are preserved,
/// and newly detected targets are added. Targets are never removed automatically.
fn sync_targets(editor: &mut TomlEditor, workspace_root: &Path) -> RailResult<Option<TargetSyncResult>> {
  use crate::targets::detect_targets_excluding;
  use crate::toml::TomlFormatter;
  use toml_edit::{DocumentMut, Item};

  // All possible config file locations to exclude from target detection
  let config_paths = [
    workspace_root.join("rail.toml"),
    workspace_root.join(".rail.toml"),
    workspace_root.join(".cargo").join("rail.toml"),
    workspace_root.join(".config").join("rail.toml"),
  ];
  let exclude_refs: Vec<&Path> = config_paths.iter().map(|p| p.as_path()).collect();

  // Detect targets in workspace, excluding all possible rail.toml locations
  let detected: BTreeSet<String> = detect_targets_excluding(workspace_root, &exclude_refs)?
    .into_iter()
    .collect();

  // Get existing targets from config
  let existing: BTreeSet<String> = editor
    .doc()
    .get("targets")
    .and_then(|v| v.as_array())
    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
    .unwrap_or_default();

  // Union merge: keep existing + add detected
  // This preserves user-configured targets and adds any newly discovered ones
  let merged: BTreeSet<String> = existing.union(&detected).cloned().collect();

  // Calculate what's actually new (targets in merged but not in existing)
  let added: Vec<_> = merged.difference(&existing).cloned().collect();

  // If nothing new to add, return None
  if added.is_empty() {
    return Ok(None);
  }

  // Use TomlFormatter for proper multiline formatting with tier grouping
  let formatter = TomlFormatter::new();
  let targets_vec: Vec<String> = merged.iter().cloned().collect();
  let formatted_array = formatter.array_targets(&targets_vec);

  // Parse the formatted array back into a TOML value
  let parse_str = format!("targets = {}", formatted_array);
  let parsed: DocumentMut = parse_str
    .parse()
    .map_err(|e| RailError::message(format!("Failed to format targets: {}", e)))?;

  // Extract the value and insert it
  if let Some(value) = parsed.get("targets").and_then(|item| item.as_value()) {
    editor.doc_mut()["targets"] = Item::Value(value.clone());
  }

  Ok(Some(TargetSyncResult {
    added,
    removed: vec![], // We never remove targets automatically
    total: merged.len(),
  }))
}

/// Print sync preview (--check mode)
fn print_sync_preview(
  config_path: &Path,
  fields_added: &[FieldChange],
  targets: &Option<TargetSyncResult>,
  has_changes: bool,
) {
  println!("config: {}", config_path.display());
  println!();

  if !fields_added.is_empty() {
    println!("would add:");
    for field in fields_added {
      println!("  [{}] = {}", field.path, field.value);
    }
    println!();
  }

  if let Some(t) = targets {
    if t.has_changes() {
      println!("targets:");
      for target in &t.added {
        println!("  + {}", target);
      }
      for target in &t.removed {
        println!("  - {}", target);
      }
      println!();
    } else {
      println!("targets: in sync ({} targets)", t.total);
    }
  } else {
    // No target detection result means they were already in sync
    println!("targets: in sync");
  }

  if has_changes {
    println!();
    println!("run without --check to apply");
  }
}

/// Print sync applied result
fn print_sync_applied(config_path: &Path, fields_added: &[FieldChange], targets: &Option<TargetSyncResult>) {
  println!("synced: {}", config_path.display());
  println!();

  if !fields_added.is_empty() {
    println!("added {} field(s):", fields_added.len());
    for field in fields_added {
      println!("  [{}] = {}", field.path, field.value);
    }
    println!();
  }

  if let Some(t) = targets {
    if t.has_changes() {
      println!("targets:");
      for target in &t.added {
        println!("  + {}", target);
      }
      for target in &t.removed {
        println!("  - {}", target);
      }
      println!("  total: {} targets", t.total);
    } else {
      println!("targets: in sync ({} targets)", t.total);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_known_unify_keys_match_schema_fields() {
    let known: BTreeSet<&str> = KNOWN_UNIFY_KEYS.iter().copied().collect();
    let from_schema: BTreeSet<&str> = schema::fields_for_section("unify").map(|field| field.key).collect();

    assert_eq!(
      known, from_schema,
      "KNOWN_UNIFY_KEYS must stay in sync with schema::SYNCABLE_FIELDS for [unify]"
    );
  }

  #[test]
  fn test_known_run_keys_cover_schema_fields() {
    let known: BTreeSet<&str> = KNOWN_RUN_KEYS.iter().copied().collect();
    let from_schema: BTreeSet<&str> = schema::fields_for_section("run").map(|field| field.key).collect();

    for field in from_schema {
      assert!(
        known.contains(field),
        "KNOWN_RUN_KEYS must include schema field [run].{}",
        field
      );
    }
  }

  #[test]
  fn test_known_release_keys_match_schema_fields() {
    let known: BTreeSet<&str> = KNOWN_RELEASE_KEYS.iter().copied().collect();
    let from_schema: BTreeSet<&str> = schema::fields_for_section("release").map(|field| field.key).collect();

    for field in from_schema {
      assert!(
        known.contains(field),
        "KNOWN_RELEASE_KEYS must include schema field [release].{}",
        field
      );
    }
    assert!(
      known.contains("changelog"),
      "KNOWN_RELEASE_KEYS must include nested [release.changelog] table"
    );
  }
}
