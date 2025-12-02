//! `cargo rail config` - Configuration management commands

use crate::commands::common::OutputFormat;
use crate::config::RailConfig;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use serde::Serialize;

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
