//! `cargo rail affected` - Show which crates are affected by changes
//!
//! This command analyzes file changes (via git) and determines:
//! - Which workspace crates directly contain changed files
//! - Which crates transitively depend on those changed crates
//! - The minimal set of crates that need testing/building

use crate::change_detection::classify::{ChangeKind, classify_file};
use crate::config::ChangeDetectionConfig;
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::graph::AffectedAnalysis;
use crate::workspace::WorkspaceContext;
use glob::Pattern;
use std::io::Write;
use std::path::PathBuf;

/// Output format for affected command
#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputFormat {
  Text,
  Json,
  NamesOnly,
  /// GitHub Actions output format for $GITHUB_OUTPUT
  GitHub,
  /// GitHub Actions matrix format for strategy.matrix
  GitHubMatrix,
  /// JSON Lines format (one object per line)
  JsonLines,
}

impl OutputFormat {
  fn from_str(s: &str) -> RailResult<Self> {
    match s.to_lowercase().as_str() {
      "text" => Ok(Self::Text),
      "json" => Ok(Self::Json),
      "names" | "names-only" => Ok(Self::NamesOnly),
      "github" => Ok(Self::GitHub),
      "github-matrix" => Ok(Self::GitHubMatrix),
      "jsonl" | "json-lines" => Ok(Self::JsonLines),
      _ => Err(RailError::message(format!(
        "Unknown format '{}'. Valid formats: text, json, names-only, github, github-matrix, jsonl",
        s
      ))),
    }
  }
}

/// Run the affected command
pub fn run_affected(
  ctx: &WorkspaceContext,
  since: Option<String>,
  from: Option<String>,
  to: Option<String>,
  format: String,
  all: bool,
  output: Option<PathBuf>,
) -> RailResult<()> {
  let output_format = OutputFormat::from_str(&format)?;

  // If --all flag is set, show all workspace crates regardless of changes
  if all {
    return display_all_crates(ctx, output_format, output.as_ref());
  }

  // Auto-detect default branch if --since not specified
  let since_ref = match since {
    Some(s) => s,
    None => detect_default_base_ref(ctx.git.git())?,
  };

  // Get changed files from git
  let changed_files = get_changed_files(ctx, &since_ref, from.as_deref(), to.as_deref())?;

  // Get change-detection config (from rail.toml or defaults)
  let cd_config = ctx
    .config
    .as_ref()
    .map(|c| c.change_detection.clone())
    .unwrap_or_default();

  // Classify files to detect infrastructure changes
  let classification = classify_changed_files(&changed_files, &cd_config);

  // Analyze affected crates
  let analysis = crate::graph::analyze(&ctx.graph, &changed_files)?;

  // Output results
  display_results(&analysis, &classification, output_format, output.as_ref())?;

  Ok(())
}

/// File classification results for change detection
#[derive(Debug, Clone)]
struct ChangeClassification {
  /// True if only documentation files changed
  docs_only: bool,
  /// True if infrastructure files changed (requires full rebuild)
  rebuild_all: bool,
  /// Infrastructure files that triggered rebuild_all
  infrastructure_files: Vec<String>,
}

/// Classify changed files to detect special cases
fn classify_changed_files(changed_files: &[PathBuf], config: &ChangeDetectionConfig) -> ChangeClassification {
  let mut docs_only = true;
  let mut rebuild_all = false;
  let mut infrastructure_files = Vec::new();

  // Compile infrastructure patterns (with glob support)
  let infra_patterns: Vec<Pattern> = config
    .infrastructure
    .iter()
    .filter_map(|p| Pattern::new(p).ok())
    .collect();

  for file in changed_files {
    let path_str = file.to_string_lossy();

    // Check for infrastructure files that trigger rebuild_all
    if is_infrastructure_file(&path_str, &infra_patterns) {
      rebuild_all = true;
      infrastructure_files.push(path_str.to_string());
      docs_only = false;
      continue;
    }

    // Check file classification
    let kind = classify_file(file);
    match kind {
      ChangeKind::Documentation => {
        // Still docs_only if all files so far are docs
      }
      _ => {
        docs_only = false;
      }
    }
  }

  // If no files changed, docs_only should be false
  if changed_files.is_empty() {
    docs_only = false;
  }

  ChangeClassification {
    docs_only,
    rebuild_all,
    infrastructure_files,
  }
}

/// Check if a file path is an infrastructure file that should trigger rebuild_all
fn is_infrastructure_file(path_str: &str, patterns: &[Pattern]) -> bool {
  for pattern in patterns {
    if pattern.matches(path_str) {
      return true;
    }
  }
  false
}

/// Display all workspace crates (--all mode)
fn display_all_crates(ctx: &WorkspaceContext, format: OutputFormat, output_file: Option<&PathBuf>) -> RailResult<()> {
  let mut all_crates: Vec<String> = ctx.graph.workspace_members();
  all_crates.sort();

  let output = match format {
    OutputFormat::Text => {
      let mut out = String::new();
      out.push_str(&format!("workspace crates: {}\n\n", all_crates.len()));
      for crate_name in &all_crates {
        out.push_str(&format!("  {}\n", crate_name));
      }
      out
    }
    OutputFormat::Json => {
      use serde_json::json;
      let output = json!({
          "crates": all_crates,
          "count": all_crates.len()
      });
      serde_json::to_string_pretty(&output)
        .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?
    }
    OutputFormat::NamesOnly => all_crates.join("\n"),
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
      eprintln!("output: {}", path.display());
    }
    None => {
      println!("{}", content);
    }
  }
  Ok(())
}

/// Get changed files from git
fn get_changed_files(
  ctx: &WorkspaceContext,
  since: &str,
  from: Option<&str>,
  to: Option<&str>,
) -> RailResult<Vec<PathBuf>> {
  // Determine git range
  let changes = if let (Some(from_ref), Some(to_ref)) = (from, to) {
    // SHA pair mode: from..to
    ctx.git.git().get_changed_files_between(from_ref, Some(to_ref))?
  } else {
    // Single ref mode: since..working tree
    ctx.git.git().get_changed_files_between(since, None)?
  };

  // Extract just the file paths (ignore status char)
  let files = changes.into_iter().map(|(path, _status)| path).collect();

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

  let output = json!({
      "changed_files": analysis.changed_files,
      "impact": {
          "direct": direct,
          "dependents": dependents,
          "test_targets": test_targets
      },
      "classification": {
          "docs_only": classification.docs_only,
          "rebuild_all": classification.rebuild_all,
          "infrastructure_files": classification.infrastructure_files
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

/// Format GitHub Actions output for $GITHUB_OUTPUT
fn format_github(analysis: &AffectedAnalysis, classification: &ChangeClassification) -> RailResult<String> {
  let (_, _, test_targets) = get_sorted_analysis(analysis);

  let test_matrix = serde_json::to_string(&test_targets)
    .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;

  Ok(format!(
    "docs_only={}\nrebuild_all={}\ntest_matrix={}\naffected_count={}\ncrates={}",
    classification.docs_only,
    classification.rebuild_all,
    test_matrix,
    test_targets.len(),
    test_targets.join(" ")
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
