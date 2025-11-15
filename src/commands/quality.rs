//! Quality analysis command
//!
//! Runs unified quality analyses over the workspace.

use crate::core::config::RailConfig;
use crate::core::context::WorkspaceContext;
use crate::core::error::{ExitCode, RailError, RailResult};
use crate::graph::workspace_graph::WorkspaceGraph;
use crate::quality::{QualityContext, create_default_engine};

/// Run quality analysis
pub fn run_quality(ctx: &WorkspaceContext, json: bool, analysis: Option<String>) -> RailResult<()> {
  // Load config
  let config = RailConfig::load(ctx.workspace_root())?;

  // Build graph with config for visibility annotations
  let graph = WorkspaceGraph::load_with_config(ctx.workspace_root(), Some(&config))?;

  // Create quality context
  let quality_ctx = QualityContext::new(ctx, &graph, &config);

  // Create engine with all analyses
  let engine = create_default_engine();

  // Run analyses
  let report = if let Some(name) = analysis {
    // Run specific analysis
    if let Some(result) = engine.run_one(&quality_ctx, &name)? {
      crate::quality::QualityReport { results: vec![result] }
    } else {
      return Err(RailError::with_help(
        format!("Unknown analysis: {}", name),
        format!(
          "Available analyses: {}",
          engine
            .analyses()
            .iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(", ")
        ),
      ));
    }
  } else {
    // Run all analyses
    engine.run_all(&quality_ctx)?
  };

  // Output results
  if json {
    println!("{}", report.to_json()?);
  } else {
    print_human_readable(&report, &engine);
  }

  // Exit with appropriate code
  if !report.passed() {
    std::process::exit(ExitCode::Validation.as_i32());
  }

  Ok(())
}

/// Print human-readable quality report
fn print_human_readable(report: &crate::quality::QualityReport, engine: &crate::quality::QualityEngine) {
  println!("🔍 Running quality analyses...\n");

  // Show registered analyses
  println!("📋 Registered analyses:");
  for analysis in engine.analyses() {
    println!("   • {}: {}", analysis.name(), analysis.description());
  }
  println!();

  // Show results
  for result in &report.results {
    let icon = if result.passed { "✅" } else { "❌" };
    println!("{} {}", icon, result.analysis);

    if !result.violations.is_empty() {
      for violation in &result.violations {
        let severity_icon = match violation.severity {
          crate::quality::Severity::Error => "❌",
          crate::quality::Severity::Warning => "⚠️ ",
          crate::quality::Severity::Info => "ℹ️ ",
        };

        println!("   {} [{}] {}", severity_icon, violation.location, violation.message);

        if let Some(ref suggestion) = violation.suggestion {
          println!("      💡 {}", suggestion);
        }
      }
    }
    println!();
  }

  // Summary
  let (errors, warnings, info) = report.count_violations();
  println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
  println!("Summary: {} errors, {} warnings, {} info", errors, warnings, info);

  if !report.passed() {
    println!("\n⚠️  Quality checks failed. Please address the violations above.");
  } else if warnings > 0 {
    println!("\n⚠️  Some warnings found. Consider addressing them.");
  } else {
    println!("\n✨ All quality checks passed!");
  }
}

/// Apply auto-fixes for quality violations
pub fn apply_fixes(ctx: &WorkspaceContext, analysis_name: &str) -> RailResult<()> {
  // Load config
  let config = RailConfig::load(ctx.workspace_root())?;

  // Build graph
  let graph = WorkspaceGraph::load_with_config(ctx.workspace_root(), Some(&config))?;

  // Create quality context
  let quality_ctx = QualityContext::new(ctx, &graph, &config);

  // Create engine
  let engine = create_default_engine();

  // Apply fixes
  let fixed_count = engine.apply_fixes(&quality_ctx, analysis_name)?;

  if fixed_count > 0 {
    println!("✅ Fixed {} violation(s)", fixed_count);
  } else {
    println!("ℹ️  No auto-fixable violations found or analysis doesn't support auto-fix");
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  #[test]
  fn test_module_exists() {
    // Smoke test to ensure module compiles
  }
}
