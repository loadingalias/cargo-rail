//! `cargo rail affected` - Show which crates are affected by changes
//!
//! This command analyzes file changes (via git) and determines:
//! - Which workspace crates directly contain changed files
//! - Which crates transitively depend on those changed crates
//! - The minimal set of crates that need testing/building

use crate::core::context::WorkspaceContext;
use crate::core::error::{RailError, RailResult};
use crate::core::vcs::SystemGit;
use crate::graph::AffectedAnalysis;
use std::path::{Path, PathBuf};

/// Output format for affected command
#[derive(Debug, Clone, Copy)]
enum OutputFormat {
  Text,
  Json,
  NamesOnly,
}

impl OutputFormat {
  fn from_str(s: &str) -> RailResult<Self> {
    match s.to_lowercase().as_str() {
      "text" => Ok(Self::Text),
      "json" => Ok(Self::Json),
      "names" | "names-only" => Ok(Self::NamesOnly),
      _ => Err(RailError::message(format!(
        "Unknown format '{}'. Valid formats: text, json, names-only",
        s
      ))),
    }
  }
}

/// Run the affected command
pub fn run_affected(
  ctx: &WorkspaceContext,
  since: String,
  from: Option<String>,
  to: Option<String>,
  format: String,
  dry_run: bool,
) -> RailResult<()> {
  let output_format = OutputFormat::from_str(&format)?;

  // Get changed files from git
  let changed_files = get_changed_files(ctx.workspace_root(), &since, from.as_deref(), to.as_deref())?;

  if dry_run {
    println!("DRY RUN: Would analyze {} changed files", changed_files.len());
    for file in &changed_files {
      println!("  - {}", file.display());
    }
    return Ok(());
  }

  // Analyze affected crates
  let analysis = crate::graph::affected::analyze(&ctx.graph, &changed_files)?;

  // Output results
  display_results(&analysis, output_format)?;

  Ok(())
}

/// Get changed files from git
fn get_changed_files(
  workspace_root: &Path,
  since: &str,
  from: Option<&str>,
  to: Option<&str>,
) -> RailResult<Vec<PathBuf>> {
  let git = SystemGit::open(workspace_root)?;

  // Determine git range
  let changes = if let (Some(from_ref), Some(to_ref)) = (from, to) {
    // SHA pair mode: from..to
    git.get_changed_files_between(from_ref, to_ref)?
  } else {
    // Single ref mode: since..HEAD
    git.get_changed_files_between(since, "HEAD")?
  };

  // Extract just the file paths (ignore status char)
  let files = changes.into_iter().map(|(path, _status)| path).collect();

  Ok(files)
}

/// Display affected analysis results
fn display_results(analysis: &AffectedAnalysis, format: OutputFormat) -> RailResult<()> {
  match format {
    OutputFormat::Text => display_text(analysis),
    OutputFormat::Json => display_json(analysis),
    OutputFormat::NamesOnly => display_names_only(analysis),
  }
}

/// Display results in human-readable text format
fn display_text(analysis: &AffectedAnalysis) -> RailResult<()> {
  println!("Affected Analysis");
  println!("=================");
  println!();

  println!("Changed files: {}", analysis.changed_files.len());
  if !analysis.changed_files.is_empty() && analysis.changed_files.len() <= 20 {
    for file in &analysis.changed_files {
      println!("  {}", file);
    }
    println!();
  }

  let direct: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.direct.iter().cloned().collect();
    v.sort();
    v
  };

  let dependents: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.dependents.iter().cloned().collect();
    v.sort();
    v
  };

  let test_targets: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
    v.sort();
    v
  };

  println!("Direct impact: {} crates", direct.len());
  for crate_name in &direct {
    println!("  📦 {}", crate_name);
  }
  println!();

  println!("Transitive dependents: {} crates", dependents.len());
  for crate_name in &dependents {
    println!("  ⬆  {}", crate_name);
  }
  println!();

  println!("Test targets (direct + dependents): {} crates", test_targets.len());
  for crate_name in &test_targets {
    println!("  🎯 {}", crate_name);
  }

  Ok(())
}

/// Display results in JSON format
fn display_json(analysis: &AffectedAnalysis) -> RailResult<()> {
  use serde_json::json;

  let direct: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.direct.iter().cloned().collect();
    v.sort();
    v
  };

  let dependents: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.dependents.iter().cloned().collect();
    v.sort();
    v
  };

  let test_targets: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
    v.sort();
    v
  };

  let output = json!({
      "changed_files": analysis.changed_files,
      "impact": {
          "direct": direct,
          "dependents": dependents,
          "test_targets": test_targets
      },
      "summary": {
          "changed_files_count": analysis.changed_files.len(),
          "direct_count": direct.len(),
          "dependents_count": dependents.len(),
          "test_targets_count": test_targets.len()
      }
  });

  println!("{}", serde_json::to_string_pretty(&output).unwrap());

  Ok(())
}

/// Display only crate names (test targets)
fn display_names_only(analysis: &AffectedAnalysis) -> RailResult<()> {
  let mut test_targets: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
  test_targets.sort();

  for crate_name in test_targets {
    println!("{}", crate_name);
  }

  Ok(())
}
