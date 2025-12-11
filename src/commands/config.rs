//! `cargo rail config` - Configuration management commands

use crate::commands::common::OutputFormat;
use crate::config::{RailConfig, schema};
use crate::error::{RailError, RailResult};
use crate::toml::TomlEditor;
use crate::workspace::WorkspaceContext;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

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
#[derive(Serialize)]
struct ValidationIssue {
  section: String,
  message: String,
}

/// Validate the configuration file
pub fn run_config_validate(ctx: &WorkspaceContext, format: OutputFormat) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let mut errors: Vec<ValidationIssue> = Vec::new();
  let mut warnings: Vec<ValidationIssue> = Vec::new();

  // Check if config exists
  let config_path = RailConfig::find_config_path(ctx.workspace_root());

  if config_path.is_none() {
    if json {
      let result = ValidationResult {
        command: "config",
        action: "validate",
        valid: false,
        config_path: None,
        errors: vec![ValidationIssue {
          section: "config".to_string(),
          message: "no configuration file found".to_string(),
        }],
        warnings: vec![],
      };
      println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| RailError::message(e.to_string()))?
      );
      // Exit with error code but no extra message (JSON has details)
      std::process::exit(2);
    } else {
      println!("no configuration file found");
      println!("\nhelp: run 'cargo rail init' to create one");
      return Err(RailError::message("no configuration file found"));
    }
  }

  let config_path = config_path.unwrap();
  let config = ctx.config.as_ref();

  if config.is_none() {
    errors.push(ValidationIssue {
      section: "config".to_string(),
      message: "configuration file exists but failed to load".to_string(),
    });
  }

  if let Some(cfg) = config {
    // Validate change detection config
    if let Err(e) = cfg.change_detection.validate() {
      errors.push(ValidationIssue {
        section: "change_detection".to_string(),
        message: e.to_string(),
      });
    }

    // Validate release config
    let workspace_members = ctx.graph.workspace_members();
    match cfg.release.validate(workspace_members) {
      Ok(release_warnings) => {
        for w in release_warnings {
          warnings.push(ValidationIssue {
            section: "release".to_string(),
            message: w,
          });
        }
      }
      Err(e) => {
        errors.push(ValidationIssue {
          section: "release".to_string(),
          message: e.to_string(),
        });
      }
    }

    // Validate per-crate split configs
    for (crate_name, crate_config) in &cfg.crates {
      if let Some(split_cfg) = &crate_config.split {
        // Check remote is set (required field)
        if split_cfg.remote.is_empty() {
          errors.push(ValidationIssue {
            section: format!("crates.{}.split", crate_name),
            message: "missing required field: remote".to_string(),
          });
        }

        // Validate branch is set
        if split_cfg.branch.is_empty() {
          warnings.push(ValidationIssue {
            section: format!("crates.{}.split", crate_name),
            message: "branch is empty, will use default".to_string(),
          });
        }
      }
    }

    // Check targets are valid (basic check)
    for target in &cfg.targets {
      if !target.contains('-') {
        warnings.push(ValidationIssue {
          section: "targets".to_string(),
          message: format!("'{}' doesn't look like a valid target triple", target),
        });
      }
    }
  }

  let valid = errors.is_empty();

  if json {
    let result = ValidationResult {
      command: "config",
      action: "validate",
      valid,
      config_path: Some(config_path.display().to_string()),
      errors,
      warnings,
    };
    println!(
      "{}",
      serde_json::to_string_pretty(&result).map_err(|e| RailError::message(e.to_string()))?
    );
  } else {
    println!("config: {}", config_path.display());
    println!();

    if !errors.is_empty() {
      println!("errors:");
      for e in &errors {
        println!("  [{}] {}", e.section, e.message);
      }
      println!();
    }

    if !warnings.is_empty() {
      println!("warnings:");
      for w in &warnings {
        println!("  [{}] {}", w.section, w.message);
      }
      println!();
    }

    if valid {
      println!("configuration is valid");
    } else {
      println!("configuration has {} error(s)", errors.len());
    }
  }

  if valid {
    Ok(())
  } else if json {
    // JSON mode: exit with error code but don't print extra message
    // (the JSON output already contains the error details)
    std::process::exit(2);
  } else {
    Err(RailError::message("configuration validation failed"))
  }
}

// ============================================================================
// Config Sync
// ============================================================================

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
pub fn run_config_sync(workspace_root: &Path, check: bool, format: OutputFormat) -> RailResult<()> {
  let json = format.is_json();

  if json {
    crate::output::set_json_mode(true);
  }

  // Find existing config
  let config_path = RailConfig::find_config_path(workspace_root).ok_or_else(|| {
    RailError::with_help(
      "no rail.toml found".to_string(),
      "run 'cargo rail init' first to create a configuration file".to_string(),
    )
  })?;

  let mut editor = TomlEditor::open(&config_path)?;
  let mut fields_added: Vec<FieldChange> = Vec::new();

  // Phase 1: Add missing fields from schema
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

  // Phase 2: Sync targets from workspace
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
      println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| RailError::message(e.to_string()))?
      );
    } else {
      print_sync_preview(&config_path, &fields_added, &targets_result, has_changes);
    }

    if has_changes {
      // Exit with code 1 to indicate changes are needed
      std::process::exit(1);
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
        println!(
          "{}",
          serde_json::to_string_pretty(&result).map_err(|e| RailError::message(e.to_string()))?
        );
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
      println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| RailError::message(e.to_string()))?
      );
    } else {
      print_sync_applied(&config_path, &fields_added, &targets_result);
    }
  }

  Ok(())
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
  if let Some(targets_item) = parsed.get("targets") {
    editor.doc_mut()["targets"] = Item::Value(targets_item.as_value().unwrap().clone());
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
