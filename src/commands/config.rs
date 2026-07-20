//! `cargo rail config` - Configuration management commands

use crate::commands::common::TextJsonOutputFormat;
use crate::config::{RailConfig, schema};
use crate::error::{RailError, RailResult};
use crate::toml::TomlEditor;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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

/// One effective configuration value and its provenance.
#[derive(Debug, Serialize)]
struct ExplainedField {
  path: String,
  configured: Option<serde_json::Value>,
  effective: serde_json::Value,
  default: Option<serde_json::Value>,
  source: String,
  classification: &'static str,
  why: &'static str,
  deprecation: Option<&'static str>,
}

#[derive(Serialize)]
struct ExplainResult {
  command: &'static str,
  action: &'static str,
  config_path: String,
  fields: Vec<ExplainedField>,
}

/// Explain effective configuration values, defaults, and provenance.
pub fn run_config_explain(
  workspace_root: &Path,
  config_override: Option<&Path>,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();
  if json {
    crate::output::set_json_mode(true);
  }

  let config_path = resolve_config_path(workspace_root, config_override)?;
  let (config, bytes) = RailConfig::load_path_with_bytes(&config_path)?;
  let content = std::str::from_utf8(&bytes)
    .map_err(|error| RailError::message(format!("{} is not valid UTF-8: {error}", config_path.display())))?;
  let configured: serde_json::Value = toml_edit::de::from_str(content)
    .map_err(|error| RailError::message(format!("failed to parse {}: {error}", config_path.display())))?;
  let configured_doc: toml_edit::DocumentMut = content
    .parse()
    .map_err(|error: toml_edit::TomlError| RailError::message(error.to_string()))?;
  let deprecations: BTreeMap<_, _> = schema::present_deprecations(&configured_doc)
    .into_iter()
    .map(|deprecation| (deprecation.path, deprecation.spec))
    .collect();
  let effective = serde_json::to_value(&config).map_err(|error| RailError::message(error.to_string()))?;
  let defaults = serde_json::to_value(RailConfig::default()).map_err(|error| RailError::message(error.to_string()))?;

  let configured = flatten_json(&configured);
  let effective = flatten_json(&effective);
  let defaults = flatten_json(&defaults);
  let paths: BTreeSet<_> = effective
    .keys()
    .chain(configured.keys())
    .filter(|path| schema::field_spec(path).is_some())
    .cloned()
    .collect();

  let fields = paths
    .into_iter()
    .filter_map(|path| {
      let field_spec = schema::field_spec(&path)?;
      let spec = deprecations.get(&path).copied().unwrap_or(field_spec);
      let configured_value = configured.get(&path).cloned();
      let effective_value = effective.get(&path).cloned().unwrap_or(serde_json::Value::Null);
      let compatibility_source = has_compatibility_source(&path, &configured);
      let source = if configured_value.is_some() || compatibility_source {
        config_path.display().to_string()
      } else {
        "default".to_string()
      };
      let default = defaults.get(&path).cloned();
      Some(ExplainedField {
        path,
        configured: configured_value,
        effective: effective_value,
        default,
        source,
        classification: spec.classification.as_str(),
        why: spec.why,
        deprecation: spec.deprecation,
      })
    })
    .collect();

  let result = ExplainResult {
    command: "config",
    action: "explain",
    config_path: config_path.display().to_string(),
    fields,
  };
  if json {
    print_config_json("explain", "success", 0, &result)
  } else {
    println!("config: {}", config_path.display());
    for field in &result.fields {
      println!("\n{}", field.path);
      println!(
        "  configured: {}",
        field
          .configured
          .as_ref()
          .map(display_json_value)
          .unwrap_or("none".to_string())
      );
      println!("  effective: {}", display_json_value(&field.effective));
      println!(
        "  default: {}",
        field
          .default
          .as_ref()
          .map(display_json_value)
          .unwrap_or("none".to_string())
      );
      println!("  source: {}", field.source);
      println!("  classification: {}", field.classification);
      println!("  why: {}", field.why);
      if let Some(deprecation) = field.deprecation {
        println!("  deprecation: {}", deprecation);
      }
    }
    Ok(())
  }
}

fn has_compatibility_source(path: &str, configured: &BTreeMap<String, serde_json::Value>) -> bool {
  let contains_any = |paths: &[&str]| paths.iter().any(|path| configured.contains_key(*path));
  match path {
    "change-detection.unknown_file_policy" => {
      configured.contains_key("change-detection.conservative_unclassified_owner_fallback")
    }
    "release.remote_effects" => contains_any(&["release.push", "release.create_github_release", "release.forge"]),
    path if path.starts_with("unify.transitive_pinning.") => {
      contains_any(&["unify.pin_transitives", "unify.transitive_host"])
    }
    path if path.starts_with("unify.msrv_policy.") => {
      contains_any(&["unify.msrv", "unify.msrv_source", "unify.enforce_msrv_inheritance"])
    }
    path if path.starts_with("run.profile.") && path.contains(".baseline.") => {
      let Some((profile, _)) = path.rsplit_once(".baseline.") else {
        return false;
      };
      configured.contains_key(&format!("{profile}.since")) || configured.contains_key(&format!("{profile}.merge_base"))
    }
    path if path.starts_with("run.profile.") && path.ends_with(".actions") => {
      let profile = path.trim_end_matches(".actions");
      configured.contains_key(&format!("{profile}.surfaces"))
    }
    _ => false,
  }
}

fn flatten_json(value: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
  fn visit(value: &serde_json::Value, path: &str, fields: &mut BTreeMap<String, serde_json::Value>) {
    match value {
      serde_json::Value::Object(object) if !object.is_empty() => {
        for (key, value) in object {
          let child = if path.is_empty() {
            key.clone()
          } else {
            format!("{path}.{key}")
          };
          visit(value, &child, fields);
        }
      }
      serde_json::Value::Array(array) if !array.is_empty() && array.iter().all(serde_json::Value::is_object) => {
        for (index, value) in array.iter().enumerate() {
          visit(value, &format!("{path}.{index}"), fields);
        }
      }
      _ if !path.is_empty() => {
        fields.insert(path.to_string(), value.clone());
      }
      _ => {}
    }
  }

  let mut fields = BTreeMap::new();
  visit(value, "", &mut fields);
  fields
}

fn display_json_value(value: &serde_json::Value) -> String {
  match value {
    serde_json::Value::String(value) => value.clone(),
    other => other.to_string(),
  }
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

    let (config, _) = RailConfig::load_path_with_bytes(&path)?;
    return Ok((config, path));
  }

  // Search standard locations
  let config_path = RailConfig::find_config_path(workspace_root).ok_or_else(|| {
    RailError::with_help(
      "no rail.toml found".to_string(),
      "run 'cargo rail init' first to create a configuration file".to_string(),
    )
  })?;

  let (config, _) = RailConfig::load_path_with_bytes(&config_path)?;
  Ok((config, config_path))
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
    for deprecation in schema::present_deprecations(doc) {
      if let Some(message) = deprecation.spec.deprecation {
        warnings.push(ValidationIssue::new(
          "compatibility",
          format!("{}: {}", deprecation.path, message),
        ));
      }
    }
  }

  // Try to load and validate semantically
  let parsed_config = toml_edit::de::from_str::<RailConfig>(&content)
    .map_err(|error| RailError::message(format!("failed to parse {}: {error}", config_path.display())));
  match parsed_config {
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
  fn visit(table: &toml_edit::Table, prefix: &str, warnings: &mut Vec<ValidationIssue>) {
    for (key, item) in table {
      let path = if prefix.is_empty() {
        key.to_string()
      } else {
        format!("{prefix}.{key}")
      };
      if !schema::is_known_path(&path) {
        let section = path.split('.').next().unwrap_or("config");
        warnings.push(ValidationIssue::new(
          section,
          format!("unknown configuration key '{path}'"),
        ));
        continue;
      }

      if let Some(child) = item.as_table() {
        visit(child, &path, warnings);
      } else if let Some(array) = item.as_array_of_tables() {
        for (index, child) in array.iter().enumerate() {
          visit(child, &format!("{path}.{index}"), warnings);
        }
      }
    }
  }

  visit(doc.as_table(), "", warnings);
}

// Config Migrate

/// One explicit semantic configuration migration.
#[derive(Debug, Clone, Serialize)]
struct MigrationChange {
  kind: &'static str,
  path: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  replacement: Option<String>,
  message: &'static str,
}

#[derive(Serialize)]
struct ConfigMigrateResult {
  command: &'static str,
  action: &'static str,
  config_path: String,
  changes: Vec<MigrationChange>,
  has_changes: bool,
}

/// Apply only versioned semantic migrations; never materialize defaults.
pub fn run_config_migrate(
  workspace_root: &Path,
  config_override: Option<&Path>,
  check: bool,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  if json {
    crate::output::set_json_mode(true);
  }

  let config_path = resolve_config_path(workspace_root, config_override)?;
  let mut editor = TomlEditor::open(&config_path)?;
  let mut changes = Vec::new();

  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.compiler_diag_cache",
    "Compiler evidence caching is now automatic.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.sort_dependencies",
    "Dependency edits are now always deterministic.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.prune_dead_features",
    "Dead-feature diagnostics are unconditional; deletion still requires closed-consumer proof.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.detect_unused",
    "Unused-dependency diagnostics are now unconditional.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.remove_unused",
    "Read-only checks and explicit apply now define the mutation boundary.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.detect_undeclared_features",
    "Borrowed-feature diagnostics are now unconditional.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "unify.fix_undeclared_features",
    "Read-only checks and explicit apply now define the mutation boundary.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "change-detection.bot_pr_confidence_profile",
    "Provider identity no longer changes planner policy.",
  );
  migrate_unify_typed_policies(&mut editor, &mut changes)?;
  migrate_release_remote_effects(&mut editor, &mut changes)?;
  migrate_run_profile_actions(&mut editor, &mut changes)?;
  migrate_run_profile_baselines(&mut editor, &mut changes)?;
  migrate_split_member_paths(workspace_root, &mut editor, &mut changes)?;
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "workspace",
    "The reserved workspace table had no behavior.",
  );
  migrate_removed_field(
    &mut editor,
    &mut changes,
    "toolchain",
    "The reserved toolchain table had no behavior.",
  );
  migrate_unknown_file_policy(&mut editor, &mut changes)?;
  migrate_reserved_sync_tables(&mut editor, &mut changes);

  let migrated = editor.doc().to_string();
  toml_edit::de::from_str::<RailConfig>(&migrated)
    .map_err(|error| RailError::message(format!("migrated configuration is invalid: {error}")))?;
  let has_changes = !changes.is_empty();

  if check {
    if json {
      let result = ConfigMigrateResult {
        command: "config",
        action: "migrate",
        config_path: config_path.display().to_string(),
        changes,
        has_changes,
      };
      print_config_json(
        "migrate",
        if has_changes { "pending_changes" } else { "success" },
        if has_changes { 1 } else { 0 },
        &result,
      )?;
    } else {
      print_migrations(&config_path, &changes, true);
    }

    if has_changes {
      return Err(RailError::CheckHasPendingChanges);
    }
    return Ok(());
  }

  if has_changes {
    editor.write()?;
  }
  if json {
    let result = ConfigMigrateResult {
      command: "config",
      action: "migrate",
      config_path: config_path.display().to_string(),
      changes,
      has_changes,
    };
    print_config_json("migrate", if has_changes { "applied" } else { "success" }, 0, &result)?;
  } else if has_changes {
    print_migrations(&config_path, &changes, false);
  } else {
    println!("config: {} (no migrations pending)", config_path.display());
  }

  Ok(())
}

fn migrate_removed_field(
  editor: &mut TomlEditor,
  changes: &mut Vec<MigrationChange>,
  path: &str,
  message: &'static str,
) {
  if editor.remove(path) {
    changes.push(MigrationChange {
      kind: "remove",
      path: path.to_string(),
      replacement: None,
      message,
    });
  }
}

fn migrate_split_member_paths(
  workspace_root: &Path,
  editor: &mut TomlEditor,
  changes: &mut Vec<MigrationChange>,
) -> RailResult<()> {
  let crate_names = editor
    .doc()
    .get("crates")
    .and_then(toml_edit::Item::as_table)
    .map(|crates| crates.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>())
    .unwrap_or_default();

  let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
  for crate_name in crate_names {
    let old = format!("crates.{crate_name}.split.paths");
    let Some(paths) = editor.get(&old).and_then(toml_edit::Item::as_array) else {
      continue;
    };
    let mut members = Vec::with_capacity(paths.len());
    for entry in paths {
      let relative = entry
        .as_inline_table()
        .and_then(|table| table.get("crate"))
        .and_then(toml_edit::Value::as_str)
        .ok_or_else(|| RailError::message(format!("{old} entries must contain one string `crate` path")))?;
      let candidate = Path::new(relative);
      if candidate.is_absolute()
        || candidate
          .components()
          .any(|component| matches!(component, std::path::Component::ParentDir))
      {
        return Err(RailError::message(format!(
          "cannot migrate split member path '{}': path must stay inside the workspace",
          relative
        )));
      }
      let package_root = crate::utils::canonicalize_existing(&workspace_root.join(candidate))?;
      if !package_root.starts_with(&workspace_root) {
        return Err(RailError::message(format!(
          "cannot migrate split member path '{}': path escapes the workspace",
          relative
        )));
      }
      let manifest_path = package_root.join("Cargo.toml");
      let manifest = std::fs::read_to_string(&manifest_path).map_err(|error| {
        RailError::message(format!(
          "cannot migrate split member path '{}': failed to read {}: {}",
          relative,
          manifest_path.display(),
          error
        ))
      })?;
      let manifest: toml_edit::DocumentMut = manifest.parse().map_err(|error: toml_edit::TomlError| {
        RailError::message(format!("cannot migrate split member path '{}': {}", relative, error))
      })?;
      let package_name = manifest
        .get("package")
        .and_then(toml_edit::Item::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml_edit::Item::as_str)
        .ok_or_else(|| {
          RailError::message(format!(
            "cannot migrate split member path '{}': Cargo.toml has no package.name",
            relative
          ))
        })?;
      members.push(package_name.to_string());
    }
    members.sort();
    members.dedup();

    let new = format!("crates.{crate_name}.split.members");
    if let Some(existing) = editor.get(&new).and_then(toml_edit::Item::as_array) {
      let mut configured = existing
        .iter()
        .map(|value| {
          value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| RailError::message(format!("{new} must contain only Cargo member names")))
        })
        .collect::<RailResult<Vec<_>>>()?;
      configured.sort();
      configured.dedup();
      if configured != members {
        return Err(RailError::message(format!(
          "cannot migrate {old}: existing {new} selects different Cargo members"
        )));
      }
    } else {
      let mut array = toml_edit::Array::new();
      for member in &members {
        array.push(member.as_str());
      }
      editor.set(&new, array)?;
    }
    editor.remove(&old);
    changes.push(MigrationChange {
      kind: "replace",
      path: old,
      replacement: Some(new),
      message: "Split ownership is now resolved from Cargo member names and the workspace snapshot.",
    });
  }
  Ok(())
}

fn migrate_unknown_file_policy(editor: &mut TomlEditor, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
  const OLD: &str = "change-detection.conservative_unclassified_owner_fallback";
  const NEW: &str = "change-detection.unknown_file_policy";
  if let Some(item) = editor.get(OLD) {
    let enabled = item
      .as_bool()
      .ok_or_else(|| RailError::message(format!("{OLD} must be a boolean before it can be migrated")))?;
    let mapped = if enabled { "owned_build_test" } else { "docs" };
    let replacement = match editor.get(NEW) {
      Some(item) => display_toml_item(item),
      None => {
        editor.set(NEW, mapped)?;
        format!("\"{mapped}\"")
      }
    };
    editor.remove(OLD);
    changes.push(MigrationChange {
      kind: "rename",
      path: OLD.to_string(),
      replacement: Some(format!("{NEW} = {replacement}")),
      message: "The legacy alias was removed; an existing explicit policy takes precedence.",
    });
  }

  if let Some(enabled) = editor.get(NEW).and_then(toml_edit::Item::as_bool) {
    let replacement = if enabled { "owned_build_test" } else { "docs" };
    editor.set(NEW, replacement)?;
    changes.push(MigrationChange {
      kind: "replace",
      path: NEW.to_string(),
      replacement: Some(format!("{NEW} = \"{replacement}\"")),
      message: "The legacy boolean is now an explicit unknown-file policy.",
    });
  }
  Ok(())
}

fn migrate_release_remote_effects(editor: &mut TomlEditor, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
  const NEW: &str = "release.remote_effects";
  const LEGACY: &[&str] = &["release.push", "release.create_github_release", "release.forge"];
  let present: Vec<_> = LEGACY
    .iter()
    .copied()
    .filter(|path| editor.contains_path(path))
    .collect();
  if present.is_empty() {
    return Ok(());
  }

  let replacement = if let Some(item) = editor.get(NEW) {
    format!("{NEW} = {}", display_toml_item(item))
  } else {
    let push = legacy_bool(editor, "release.push")?.unwrap_or(false);
    let create_release = legacy_bool(editor, "release.create_github_release")?.unwrap_or(false);
    if create_release && !push {
      return Err(RailError::with_help(
        "cannot migrate release.create_github_release = true with release.push = false",
        "choose one explicit policy first: set release.push = true to create a forge release, or set release.create_github_release = false",
      ));
    }
    let forge = editor
      .get("release.forge")
      .map(|item| {
        item
          .as_str()
          .map(str::to_string)
          .ok_or_else(|| RailError::message("release.forge must be a string before it can be migrated"))
      })
      .transpose()?
      .unwrap_or_else(|| "auto".to_string());
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
      editor.set(NEW, remote_effects.as_str())?;
      format!("{NEW} = \"{remote_effects}\"")
    }
  };

  for path in present {
    editor.remove(path);
    changes.push(MigrationChange {
      kind: "merge",
      path: path.to_string(),
      replacement: Some(replacement.clone()),
      message: "The legacy release-effect matrix is now one typed policy.",
    });
  }
  Ok(())
}

fn migrate_unify_typed_policies(editor: &mut TomlEditor, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
  let Some(unify) = editor
    .doc_mut()
    .get_mut("unify")
    .and_then(toml_edit::Item::as_table_mut)
  else {
    return Ok(());
  };

  let legacy_transitive: Vec<_> = ["pin_transitives", "transitive_host"]
    .into_iter()
    .filter(|key| unify.contains_key(key))
    .collect();
  if !legacy_transitive.is_empty() {
    let replacement = if let Some(item) = unify.get("transitive_pinning") {
      format!("unify.transitive_pinning = {}", display_toml_item(item))
    } else {
      let enabled = legacy_table_bool(unify, "pin_transitives")?.unwrap_or(false);
      let host = unify
        .get("transitive_host")
        .map(|item| {
          item
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| RailError::message("unify.transitive_host must be a string before it can be migrated"))
        })
        .transpose()?
        .unwrap_or_else(|| "root".to_string());
      if enabled {
        let mut policy = toml_edit::InlineTable::new();
        policy.insert("host", host.into());
        let value = toml_edit::Value::InlineTable(policy);
        let replacement = value.to_string();
        unify.insert("transitive_pinning", toml_edit::Item::Value(value));
        format!("unify.transitive_pinning = {replacement}")
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

  let legacy_msrv: Vec<_> = ["msrv", "msrv_source", "enforce_msrv_inheritance"]
    .into_iter()
    .filter(|key| unify.contains_key(key))
    .collect();
  if !legacy_msrv.is_empty() {
    let replacement = if let Some(item) = unify.get("msrv_policy") {
      format!("unify.msrv_policy = {}", display_toml_item(item))
    } else {
      let enabled = legacy_table_bool(unify, "msrv")?.unwrap_or(true);
      let inherit = legacy_table_bool(unify, "enforce_msrv_inheritance")?.unwrap_or(false);
      if !enabled && inherit {
        return Err(RailError::with_help(
          "cannot migrate enforce_msrv_inheritance = true with msrv = false",
          "choose one explicit policy first: enable MSRV computation or disable inheritance",
        ));
      }
      let source = unify
        .get("msrv_source")
        .map(|item| {
          item
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| RailError::message("unify.msrv_source must be a string before it can be migrated"))
        })
        .transpose()?
        .unwrap_or_else(|| "max".to_string());
      if !matches!(source.as_str(), "deps" | "workspace" | "max") {
        return Err(RailError::message(format!(
          "unify.msrv_source has unsupported value '{source}'"
        )));
      }

      if enabled && source == "max" && !inherit {
        "field omitted (MSRV compute/max defaults apply)".to_string()
      } else {
        let mut policy = toml_edit::InlineTable::new();
        policy.insert("mode", (if enabled { "compute" } else { "disabled" }).into());
        if enabled {
          policy.insert("source", source.into());
          if inherit {
            policy.insert("inherit", true.into());
          }
        }
        let value = toml_edit::Value::InlineTable(policy);
        let replacement = value.to_string();
        unify.insert("msrv_policy", toml_edit::Item::Value(value));
        format!("unify.msrv_policy = {replacement}")
      }
    };
    for key in legacy_msrv {
      unify.remove(key);
      changes.push(MigrationChange {
        kind: "merge",
        path: format!("unify.{key}"),
        replacement: Some(replacement.clone()),
        message: "The legacy MSRV booleans and source are now one typed policy.",
      });
    }
  }

  if unify.is_empty() {
    editor.doc_mut().as_table_mut().remove("unify");
  }
  Ok(())
}

fn legacy_table_bool(table: &toml_edit::Table, key: &str) -> RailResult<Option<bool>> {
  table
    .get(key)
    .map(|item| {
      item
        .as_bool()
        .ok_or_else(|| RailError::message(format!("unify.{key} must be a boolean before it can be migrated")))
    })
    .transpose()
}

fn migrate_run_profile_baselines(editor: &mut TomlEditor, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
  let profile_names: Vec<String> = editor
    .doc()
    .get("run")
    .and_then(toml_edit::Item::as_table)
    .and_then(|run| run.get("profile"))
    .and_then(toml_edit::Item::as_table)
    .map(|profiles| profiles.iter().map(|(name, _)| name.to_string()).collect())
    .unwrap_or_default();

  for profile_name in profile_names {
    let profile = editor
      .doc_mut()
      .get_mut("run")
      .and_then(toml_edit::Item::as_table_mut)
      .and_then(|run| run.get_mut("profile"))
      .and_then(toml_edit::Item::as_table_mut)
      .and_then(|profiles| profiles.get_mut(&profile_name))
      .and_then(toml_edit::Item::as_table_mut)
      .ok_or_else(|| RailError::message(format!("run.profile.{profile_name} must be a table")))?;
    let has_since = profile.contains_key("since");
    let has_merge_base = profile.contains_key("merge_base");
    if !has_since && !has_merge_base {
      continue;
    }

    let since = profile
      .get("since")
      .map(|item| {
        item
          .as_str()
          .map(str::to_string)
          .ok_or_else(|| RailError::message(format!("run.profile.{profile_name}.since must be a string")))
      })
      .transpose()?;
    let merge_base = profile
      .get("merge_base")
      .map(|item| {
        item
          .as_bool()
          .ok_or_else(|| RailError::message(format!("run.profile.{profile_name}.merge_base must be a boolean")))
      })
      .transpose()?;
    if since.is_some() && merge_base == Some(true) && !profile.contains_key("baseline") {
      return Err(RailError::with_help(
        format!("cannot migrate conflicting baseline in run.profile.{profile_name}"),
        "remove either since or merge_base = true so the profile selects one baseline mode",
      ));
    }

    let replacement = if let Some(item) = profile.get("baseline") {
      format!("run.profile.{profile_name}.baseline = {}", display_toml_item(item))
    } else if let Some(reference) = since {
      let mut baseline = toml_edit::InlineTable::new();
      baseline.insert("kind", "since".into());
      baseline.insert("reference", reference.into());
      let value = toml_edit::Value::InlineTable(baseline);
      let replacement = value.to_string();
      profile.insert("baseline", toml_edit::Item::Value(value));
      format!("run.profile.{profile_name}.baseline = {replacement}")
    } else if merge_base == Some(true) {
      let mut baseline = toml_edit::InlineTable::new();
      baseline.insert("kind", "merge-base".into());
      let value = toml_edit::Value::InlineTable(baseline);
      let replacement = value.to_string();
      profile.insert("baseline", toml_edit::Item::Value(value));
      format!("run.profile.{profile_name}.baseline = {replacement}")
    } else {
      "field omitted (the profile has no baseline by default)".to_string()
    };

    for key in ["since", "merge_base"] {
      if profile.remove(key).is_some() {
        changes.push(MigrationChange {
          kind: "merge",
          path: format!("run.profile.{profile_name}.{key}"),
          replacement: Some(replacement.clone()),
          message: "The legacy baseline pair is now one typed policy.",
        });
      }
    }
  }
  Ok(())
}

fn migrate_run_profile_actions(editor: &mut TomlEditor, changes: &mut Vec<MigrationChange>) -> RailResult<()> {
  let profile_names: Vec<String> = editor
    .doc()
    .get("run")
    .and_then(toml_edit::Item::as_table)
    .and_then(|run| run.get("profile"))
    .and_then(toml_edit::Item::as_table)
    .map(|profiles| profiles.iter().map(|(name, _)| name.to_string()).collect())
    .unwrap_or_default();

  for profile_name in profile_names {
    let profile = editor
      .doc_mut()
      .get_mut("run")
      .and_then(toml_edit::Item::as_table_mut)
      .and_then(|run| run.get_mut("profile"))
      .and_then(toml_edit::Item::as_table_mut)
      .and_then(|profiles| profiles.get_mut(&profile_name))
      .and_then(toml_edit::Item::as_table_mut)
      .ok_or_else(|| RailError::message(format!("run.profile.{profile_name} must be a table")))?;
    if !profile.contains_key("surfaces") {
      continue;
    }
    if profile.contains_key("actions") {
      return Err(RailError::with_help(
        format!("cannot migrate conflicting selection in run.profile.{profile_name}"),
        "remove either actions or deprecated surfaces so the profile has one ordered action list",
      ));
    }
    let surfaces = profile.remove("surfaces").ok_or_else(|| {
      RailError::message(format!(
        "run.profile.{profile_name}.surfaces disappeared during migration"
      ))
    })?;
    let replacement = surfaces
      .as_value()
      .map(|value| {
        let mut value = value.clone();
        value.decor_mut().clear();
        value.to_string()
      })
      .unwrap_or_else(|| surfaces.to_string());
    profile.insert("actions", surfaces);
    changes.push(MigrationChange {
      kind: "rename",
      path: format!("run.profile.{profile_name}.surfaces"),
      replacement: Some(format!("run.profile.{profile_name}.actions = {replacement}")),
      message: "Executable profile selections are action IDs; planner surfaces remain impact outputs.",
    });
  }
  Ok(())
}

fn legacy_bool(editor: &TomlEditor, path: &str) -> RailResult<Option<bool>> {
  editor
    .get(path)
    .map(|item| {
      item
        .as_bool()
        .ok_or_else(|| RailError::message(format!("{path} must be a boolean before it can be migrated")))
    })
    .transpose()
}

fn display_toml_item(item: &toml_edit::Item) -> String {
  item
    .as_value()
    .map(ToString::to_string)
    .unwrap_or_else(|| item.to_string())
}

fn migrate_reserved_sync_tables(editor: &mut TomlEditor, changes: &mut Vec<MigrationChange>) {
  let crate_names: Vec<String> = editor
    .doc()
    .get("crates")
    .and_then(toml_edit::Item::as_table)
    .map(|crates| crates.iter().map(|(name, _)| name.to_string()).collect())
    .unwrap_or_default();
  for crate_name in crate_names {
    let path = format!("crates.{crate_name}.sync");
    migrate_removed_field(
      editor,
      changes,
      &path,
      "The reserved per-crate sync table had no behavior.",
    );
  }
}

fn print_migrations(config_path: &Path, changes: &[MigrationChange], check: bool) {
  println!("config: {}", config_path.display());
  if changes.is_empty() {
    println!("no migrations pending");
    return;
  }
  println!();
  for change in changes {
    let verb = if check { "would migrate" } else { "migrated" };
    if let Some(replacement) = &change.replacement {
      println!("{verb}: {} -> {}", change.path, replacement);
    } else {
      println!("{verb}: {} ({})", change.path, change.message);
    }
  }
  if check {
    println!("\nrun without --check to apply");
  }
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
