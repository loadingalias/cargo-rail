//! `cargo rail affected` - Show which crates are affected by changes
//!
//! This command analyzes file changes (via git) and determines:
//! - Which workspace crates directly contain changed files
//! - Which crates transitively depend on those changed crates
//! - The minimal set of crates that need testing/building

use crate::change_detection::{ChangeClassification, ChangeClassifier};
use crate::commands::common::OutputFormat;
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::graph::AffectedAnalysis;
use crate::progress;
use crate::workspace::WorkspaceContext;
use std::io::Write;
use std::path::PathBuf;

/// Options for the `affected` command
#[derive(Debug, Default)]
pub struct AffectedOptions {
  /// Git ref to compare against (e.g., "main", "HEAD~5")
  pub since: Option<String>,
  /// Start of SHA range (used with `to` for SHA pair mode)
  pub from: Option<String>,
  /// End of SHA range (used with `from` for SHA pair mode)
  pub to: Option<String>,
  /// Output format
  pub format: OutputFormat,
  /// Show all workspace crates regardless of changes
  pub all: bool,
  /// Write output to file instead of stdout
  pub output: Option<PathBuf>,
  /// Show detailed explanation of why crates are affected
  pub explain: bool,
}

/// Run the affected command
pub fn run_affected(ctx: &WorkspaceContext, opts: AffectedOptions) -> RailResult<()> {
  // JSON-like formats enable structured error output and suppress progress
  if opts.format.is_json_like() {
    crate::output::set_json_mode(true);
  }

  // If --all flag is set, show all workspace crates regardless of changes
  if opts.all {
    return display_all_crates(ctx, opts.format, opts.output.as_ref());
  }

  // Auto-detect default branch if --since not specified
  let since_ref = match opts.since {
    Some(s) => s,
    None => detect_default_base_ref(ctx.git.git())?,
  };

  // Get changed files from git
  let changed_files = get_changed_files(ctx, &since_ref, opts.from.as_deref(), opts.to.as_deref())?;

  // Create classifier from config (or defaults)
  let classifier = ctx
    .config
    .as_ref()
    .map(|c| ChangeClassifier::new(&c.change_detection))
    .unwrap_or_else(ChangeClassifier::default_config);

  // Classify files to detect infrastructure changes
  let classification = classifier.classify(&changed_files);

  // Analyze affected crates
  let analysis = crate::graph::analyze(&ctx.graph, &changed_files)?;

  // Explain mode: detailed breakdown of why crates are affected
  if opts.explain {
    return display_explain(ctx, &changed_files, &analysis, &classification);
  }

  // Output results
  display_results(&analysis, &classification, opts.format, opts.output.as_ref())?;

  Ok(())
}

/// Display all workspace crates (--all mode)
fn display_all_crates(ctx: &WorkspaceContext, format: OutputFormat, output_file: Option<&PathBuf>) -> RailResult<()> {
  let all_crates = ctx.graph.workspace_members();

  let output = match format {
    OutputFormat::Text => {
      let mut out = String::new();
      out.push_str(&format!("workspace crates: {}\n\n", all_crates.len()));
      for crate_name in all_crates {
        out.push_str(&format!("  {}\n", crate_name));
      }
      out
    }
    OutputFormat::Json => {
      use serde_json::json;
      let output = json!({
          "command": "affected",
          "all": true,
          "crates": all_crates,
          "count": all_crates.len()
      });
      serde_json::to_string_pretty(&output)
        .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?
    }
    OutputFormat::NamesOnly => all_crates.join("\n"),
    OutputFormat::CargoArgs => format_cargo_args_list(all_crates),
    OutputFormat::GitHub => {
      let test_matrix = serde_json::to_string(&all_crates)
        .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
      format!(
        "docs_only=false\nrebuild_all=false\ntest_matrix={}\naffected_count={}\ncrates={}",
        test_matrix,
        all_crates.len(),
        all_crates.join(" ")
      )
    }
    OutputFormat::GitHubMatrix => {
      use serde_json::json;
      let matrix = json!({
          "include": all_crates.iter().map(|c| json!({"crate": c})).collect::<Vec<_>>()
      });
      serde_json::to_string(&matrix).map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?
    }
    OutputFormat::JsonLines => all_crates
      .iter()
      .map(|c| {
        serde_json::to_string(&serde_json::json!({"crate": c}))
          .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))
      })
      .collect::<RailResult<Vec<_>>>()?
      .join("\n"),
  };

  write_output(&output, output_file)?;
  Ok(())
}

/// Display detailed explanation of why crates are affected
fn display_explain(
  ctx: &WorkspaceContext,
  changed_files: &[PathBuf],
  analysis: &AffectedAnalysis,
  classification: &ChangeClassification,
) -> RailResult<()> {
  use std::collections::HashMap;

  // Special cases first
  if classification.docs_only {
    println!("docs-only changes (no tests required)");
    return Ok(());
  }

  if classification.rebuild_all {
    println!("infrastructure changes detected (full rebuild recommended):\n");
    for file in &classification.infrastructure_files {
      println!("  {}", file);
    }
    println!();
  }

  // Group changed files by owning crate
  let mut files_by_crate: HashMap<String, Vec<&PathBuf>> = HashMap::new();
  let mut unowned_files = Vec::new();

  for file in changed_files {
    let crates = ctx.graph.files_to_crates(&[file]);
    if crates.is_empty() {
      unowned_files.push(file);
    } else {
      for crate_name in crates {
        files_by_crate.entry(crate_name).or_default().push(file);
      }
    }
  }

  // Show changed files summary
  println!("changed files: {}", changed_files.len());
  if !unowned_files.is_empty() {
    println!("  (non-crate): {} files", unowned_files.len());
  }
  println!();

  // Show direct crates with their changed files
  let (direct, dependents, _) = get_sorted_analysis(analysis);

  if !direct.is_empty() {
    println!("direct ({}):", direct.len());
    for crate_name in &direct {
      if let Some(files) = files_by_crate.get(crate_name) {
        println!("  {} ({} files)", crate_name, files.len());
        for file in files.iter().take(3) {
          println!("    {}", file.display());
        }
        if files.len() > 3 {
          println!("    ... {} more", files.len() - 3);
        }
      } else {
        println!("  {}", crate_name);
      }
    }
    println!();
  }

  // Show transitive dependents with reason
  if !dependents.is_empty() {
    println!("transitive ({}):", dependents.len());
    for crate_name in &dependents {
      // Find which direct crate(s) this depends on
      let mut reasons: Vec<&String> = Vec::new();
      for direct_crate in &direct {
        if let Ok(deps) = ctx.graph.transitive_dependents(direct_crate)
          && deps.contains(crate_name)
        {
          reasons.push(direct_crate);
        }
      }

      if reasons.is_empty() {
        println!("  {}", crate_name);
      } else if reasons.len() == 1 {
        println!("  {} (depends on {})", crate_name, reasons[0]);
      } else {
        let reasons_str: Vec<&str> = reasons.iter().map(|s| s.as_str()).collect();
        println!("  {} (depends on {})", crate_name, reasons_str.join(", "));
      }
    }
    println!();
  }

  // Summary
  let test_count = direct.len() + dependents.len();
  println!("test targets: {}", test_count);

  Ok(())
}

/// Write output to stdout or file
fn write_output(content: &str, output_file: Option<&PathBuf>) -> RailResult<()> {
  match output_file {
    Some(path) => {
      // Write to file (append mode for GITHUB_OUTPUT compatibility)
      let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| RailError::message(format!("failed to open '{}': {}", path.display(), e)))?;
      writeln!(file, "{}", content)
        .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      progress!("output: {}", path.display());
    }
    None => {
      println!("{}", content);
    }
  }
  Ok(())
}

/// Get changed files from git, normalized to workspace-relative paths
fn get_changed_files(
  ctx: &WorkspaceContext,
  since: &str,
  from: Option<&str>,
  to: Option<&str>,
) -> RailResult<Vec<PathBuf>> {
  // Determine git range
  let git_changes = if let (Some(from_ref), Some(to_ref)) = (from, to) {
    // SHA pair mode: from..to
    ctx.git.git().get_changed_files_between(from_ref, Some(to_ref))?
  } else {
    // Single ref mode: since..working tree
    ctx.git.git().get_changed_files_between(since, None)?
  };

  // Convert git-relative paths to workspace-relative paths
  // This handles nested workspaces (e.g., git at /repo, workspace at /repo/rust)
  let files = git_changes
    .into_iter()
    .filter_map(|(git_path, _status)| ctx.to_workspace_path(&git_path))
    .collect();

  Ok(files)
}

/// Display affected analysis results
fn display_results(
  analysis: &AffectedAnalysis,
  classification: &ChangeClassification,
  format: OutputFormat,
  output_file: Option<&PathBuf>,
) -> RailResult<()> {
  let output = match format {
    OutputFormat::Text => format_text(analysis, classification),
    OutputFormat::Json => format_json(analysis, classification)?,
    OutputFormat::NamesOnly => format_names_only(analysis),
    OutputFormat::CargoArgs => format_cargo_args(analysis),
    OutputFormat::GitHub => format_github(analysis, classification)?,
    OutputFormat::GitHubMatrix => format_github_matrix(analysis)?,
    OutputFormat::JsonLines => format_jsonl(analysis, classification)?,
  };

  write_output(&output, output_file)
}

/// Helper to get sorted vectors from analysis
fn get_sorted_analysis(analysis: &AffectedAnalysis) -> (Vec<String>, Vec<String>, Vec<String>) {
  let direct = {
    let mut v: Vec<_> = analysis.impact.direct.iter().cloned().collect();
    v.sort();
    v
  };
  let dependents = {
    let mut v: Vec<_> = analysis.impact.dependents.iter().cloned().collect();
    v.sort();
    v
  };
  let test_targets = {
    let mut v: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
    v.sort();
    v
  };
  (direct, dependents, test_targets)
}

/// Format results in human-readable text format
fn format_text(analysis: &AffectedAnalysis, classification: &ChangeClassification) -> String {
  let (direct, dependents, test_targets) = get_sorted_analysis(analysis);

  let mut out = String::new();

  // Show classification info
  if classification.docs_only {
    out.push_str("docs-only changes (no tests required)\n\n");
    return out;
  }
  if classification.rebuild_all {
    out.push_str("infrastructure changes (full rebuild recommended):\n");
    for file in &classification.infrastructure_files {
      out.push_str(&format!("  {}\n", file));
    }
    out.push('\n');
  }

  // Issue #22: Show custom category matches
  if !classification.custom_categories.is_empty() {
    out.push_str("custom categories:\n");
    let mut categories: Vec<_> = classification.custom_categories.keys().collect();
    categories.sort();
    for category in categories {
      let files = &classification.custom_categories[category];
      out.push_str(&format!("  {} ({} files):\n", category, files.len()));
      for file in files.iter().take(5) {
        out.push_str(&format!("    {}\n", file));
      }
      if files.len() > 5 {
        out.push_str(&format!("    ... and {} more\n", files.len() - 5));
      }
    }
    out.push('\n');
  }

  out.push_str(&format!("changed files: {}\n", analysis.changed_files.len()));
  if !analysis.changed_files.is_empty() && analysis.changed_files.len() <= 20 {
    for file in &analysis.changed_files {
      out.push_str(&format!("  {}\n", file));
    }
    out.push('\n');
  }

  if !direct.is_empty() {
    out.push_str(&format!("direct: {}\n", direct.len()));
    for crate_name in &direct {
      out.push_str(&format!("  {}\n", crate_name));
    }
    out.push('\n');
  }

  if !dependents.is_empty() {
    out.push_str(&format!("transitive: {}\n", dependents.len()));
    for crate_name in &dependents {
      out.push_str(&format!("  {}\n", crate_name));
    }
    out.push('\n');
  }

  out.push_str(&format!("test targets: {}\n", test_targets.len()));
  for crate_name in &test_targets {
    out.push_str(&format!("  {}\n", crate_name));
  }

  out
}

/// Format results in JSON format
fn format_json(analysis: &AffectedAnalysis, classification: &ChangeClassification) -> RailResult<String> {
  use serde_json::json;

  let (direct, dependents, test_targets) = get_sorted_analysis(analysis);

  // Issue #22: Include custom categories in JSON output
  let output = json!({
      "command": "affected",
      "changed_files": analysis.changed_files,
      "impact": {
          "direct": direct,
          "dependents": dependents,
          "test_targets": test_targets
      },
      "classification": {
          "docs_only": classification.docs_only,
          "rebuild_all": classification.rebuild_all,
          "infrastructure_files": classification.infrastructure_files,
          "custom_categories": classification.custom_categories
      },
      "summary": {
          "changed_files_count": analysis.changed_files.len(),
          "direct_count": direct.len(),
          "dependents_count": dependents.len(),
          "test_targets_count": test_targets.len()
      }
  });

  serde_json::to_string_pretty(&output).map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))
}

/// Format only crate names (test targets)
fn format_names_only(analysis: &AffectedAnalysis) -> String {
  let mut test_targets: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
  test_targets.sort();
  test_targets.join("\n")
}

/// Format cargo -p args for direct use with cargo commands
///
/// Output: `-p crate1 -p crate2 -p crate3`
/// Usage: `cargo test $(cargo rail affected -f cargo-args)`
fn format_cargo_args(analysis: &AffectedAnalysis) -> String {
  let mut test_targets: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
  test_targets.sort();
  format_cargo_args_list(&test_targets)
}

/// Format a list of crate names as cargo -p args
fn format_cargo_args_list(crates: &[String]) -> String {
  if crates.is_empty() {
    String::new()
  } else {
    crates.iter().map(|c| format!("-p {}", c)).collect::<Vec<_>>().join(" ")
  }
}

/// Format GitHub Actions output for $GITHUB_OUTPUT
///
/// Outputs all fields that GitHub Actions workflows commonly need:
/// - Core: crates, affected_count, test_matrix
/// - Classification: docs_only, rebuild_all
/// - Details: direct, transitive, changed_files_count
/// - Infrastructure: infrastructure_files (JSON array)
/// - Custom: custom_categories (JSON object)
fn format_github(analysis: &AffectedAnalysis, classification: &ChangeClassification) -> RailResult<String> {
  let (direct, dependents, test_targets) = get_sorted_analysis(analysis);

  let test_matrix = serde_json::to_string(&test_targets)
    .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;

  let infrastructure_files = serde_json::to_string(&classification.infrastructure_files)
    .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;

  let custom_categories = serde_json::to_string(&classification.custom_categories)
    .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;

  Ok(format!(
    "docs_only={}\n\
     rebuild_all={}\n\
     test_matrix={}\n\
     affected_count={}\n\
     crates={}\n\
     direct={}\n\
     transitive={}\n\
     changed_files_count={}\n\
     infrastructure_files={}\n\
     custom_categories={}",
    classification.docs_only,
    classification.rebuild_all,
    test_matrix,
    test_targets.len(),
    test_targets.join(" "),
    direct.join(" "),
    dependents.join(" "),
    analysis.changed_files.len(),
    infrastructure_files,
    custom_categories,
  ))
}

/// Format GitHub Actions strategy.matrix output
fn format_github_matrix(analysis: &AffectedAnalysis) -> RailResult<String> {
  use serde_json::json;

  let (_, _, test_targets) = get_sorted_analysis(analysis);

  let matrix = json!({
      "include": test_targets.iter().map(|c| json!({"crate": c})).collect::<Vec<_>>()
  });

  serde_json::to_string(&matrix).map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))
}

/// Format JSON Lines output (one JSON object per line)
fn format_jsonl(analysis: &AffectedAnalysis, classification: &ChangeClassification) -> RailResult<String> {
  use serde_json::json;

  let (direct, dependents, test_targets) = get_sorted_analysis(analysis);

  let meta = json!({
      "type": "metadata",
      "docs_only": classification.docs_only,
      "rebuild_all": classification.rebuild_all,
      "changed_files_count": analysis.changed_files.len(),
      "affected_count": test_targets.len()
  });

  let mut lines =
    vec![serde_json::to_string(&meta).map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?];

  for crate_name in &test_targets {
    let is_direct = direct.contains(crate_name);
    let is_dependent = dependents.contains(crate_name);
    let crate_obj = json!({
        "type": "crate",
        "name": crate_name,
        "direct": is_direct,
        "transitive": is_dependent && !is_direct
    });
    lines.push(
      serde_json::to_string(&crate_obj).map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?,
    );
  }

  Ok(lines.join("\n"))
}
