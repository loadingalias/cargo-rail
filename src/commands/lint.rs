//! Lint command implementation

use crate::checks::{CheckContext, Severity, create_manifest_runner};
use crate::core::context::WorkspaceContext;
use crate::core::error::{RailError, RailResult};
use crate::lint::{DepsLinter, DepsReport, VersionsLinter, VersionsReport};
use cargo_metadata::MetadataCommand;

/// Run the lint deps command
pub fn run_lint_deps(ctx: &WorkspaceContext, fix: bool, apply: bool, json: bool, strict: bool) -> RailResult<()> {
  // Load workspace metadata
  let metadata = MetadataCommand::new()
    .current_dir(ctx.workspace_root())
    .exec()
    .map_err(|e| RailError::message(format!("Failed to load workspace metadata: {}", e)))?;

  let linter = DepsLinter::new(metadata);

  // Analyze dependencies
  let report = linter.analyze()?;

  if fix {
    // Apply fixes
    let fix_report = linter.fix(&report, apply)?;

    if json {
      println!("{}", serde_json::to_string_pretty(&fix_report)?);
    } else {
      print_fix_report(&fix_report);
    }

    // Exit with error in strict mode if issues found
    if strict && fix_report.total_fixed > 0 {
      std::process::exit(1);
    }
  } else {
    // Just report issues
    if json {
      println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
      print_deps_report(&report);
    }

    // Exit with error in strict mode if issues found
    if strict && report.total_issues > 0 {
      std::process::exit(1);
    }
  }

  Ok(())
}

fn print_deps_report(report: &DepsReport) {
  if report.total_issues == 0 {
    println!("✅ No dependency issues found");
    println!("   All workspace dependencies use proper inheritance");
    return;
  }

  println!("⚠️  Found {} dependency issue(s)", report.total_issues);
  println!();

  // Group by crate
  let mut by_crate: std::collections::HashMap<String, Vec<&crate::lint::DepsIssue>> = std::collections::HashMap::new();
  for issue in &report.issues {
    by_crate.entry(issue.crate_name.clone()).or_default().push(issue);
  }

  for (crate_name, issues) in by_crate.iter() {
    println!("📦 {}", crate_name);
    for issue in issues {
      println!(
        "   {} → {} should use workspace inheritance",
        issue.section, issue.dependency_name
      );
      println!("      Current: {}", issue.current_spec);
      println!("      Fix:     {}", issue.suggested_fix);
      println!();
    }
  }

  println!("💡 Affected dependencies:");
  for dep in &report.affected_dependencies {
    println!("   - {}", dep);
  }
  println!();

  println!("To fix these issues:");
  println!("  cargo rail lint deps --fix          (dry-run)");
  println!("  cargo rail lint deps --fix --apply  (apply changes)");
}

fn print_fix_report(report: &crate::lint::DepsFixReport) {
  if report.dry_run {
    println!("🔍 Dry-run mode (no changes applied)");
  } else {
    println!("✅ Applied fixes");
  }
  println!();

  if report.total_fixed == 0 {
    println!("No fixes needed");
    return;
  }

  println!("Fixed {} issue(s):", report.total_fixed);
  println!();

  // Group by crate
  let mut by_crate: std::collections::HashMap<String, Vec<&crate::lint::FixedIssue>> = std::collections::HashMap::new();
  for fix in &report.fixed {
    by_crate.entry(fix.crate_name.clone()).or_default().push(fix);
  }

  for (crate_name, fixes) in by_crate.iter() {
    println!("📦 {}", crate_name);
    for fix in fixes {
      println!("   {} → {}", fix.section, fix.dependency_name);
      println!("      - {}", fix.before);
      println!("      + {}", fix.after);
      println!();
    }
  }

  if report.dry_run {
    println!("To apply these changes:");
    println!("  cargo rail lint deps --fix --apply");
  }
}

/// Run the lint versions command
pub fn run_lint_versions(ctx: &WorkspaceContext, fix: bool, apply: bool, json: bool, strict: bool) -> RailResult<()> {
  // Load workspace metadata
  let metadata = MetadataCommand::new()
    .current_dir(ctx.workspace_root())
    .exec()
    .map_err(|e| RailError::message(format!("Failed to load workspace metadata: {}", e)))?;

  // Try to load policy config
  let config = ctx.config.as_ref();
  let policy = config.map(|c| c.policy.clone());

  let linter = VersionsLinter::new(metadata, policy);

  // Analyze version conflicts
  let report = linter.analyze()?;

  if fix {
    // Apply fixes
    let fix_report = linter.fix(&report, apply)?;

    if json {
      println!("{}", serde_json::to_string_pretty(&fix_report)?);
    } else {
      print_versions_fix_report(&fix_report);
    }

    // Exit with error in strict mode if issues found
    if strict && fix_report.total_fixed > 0 {
      std::process::exit(1);
    }
  } else {
    // Just report issues
    if json {
      println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
      print_versions_report(&report);
    }

    // Exit with error in strict mode if forbidden conflicts found
    if strict && report.forbidden_count > 0 {
      std::process::exit(1);
    }
  }

  Ok(())
}

fn print_versions_report(report: &VersionsReport) {
  if report.total_conflicts == 0 {
    println!("✅ No version conflicts found");
    println!("   All dependencies have unified versions");
    return;
  }

  println!(
    "⚠️  Found {} dependencies with multiple versions",
    report.total_conflicts
  );
  if report.forbidden_count > 0 {
    println!("   🚫 {} are explicitly forbidden by policy", report.forbidden_count);
  }
  println!();

  for issue in &report.issues {
    let prefix = if issue.is_forbidden { "🚫" } else { "⚠️ " };
    println!(
      "{} {} (found {} versions)",
      prefix,
      issue.dependency_name,
      issue.versions.len()
    );

    for version in &issue.versions {
      if let Some(crates) = issue.usage_by_version.get(version) {
        println!("   v{} used by:", version);
        for crate_name in crates {
          println!("      - {}", crate_name);
        }
      }
    }

    println!("   💡 Suggested unified version: {}", issue.suggested_version);
    println!();
  }

  println!("To fix these conflicts:");
  println!("  cargo rail lint versions --fix          (dry-run)");
  println!("  cargo rail lint versions --fix --apply  (apply changes)");
  println!();

  if report.forbidden_count > 0 {
    println!(
      "⚠️  Policy violation: {} forbidden conflicts must be resolved",
      report.forbidden_count
    );
    println!("   Configure in rail.toml: [policy] forbid_multiple_versions = [...]");
  }
}

fn print_versions_fix_report(report: &crate::lint::VersionsFixReport) {
  if report.dry_run {
    println!("🔍 Dry-run mode (no changes applied)");
  } else {
    println!("✅ Applied fixes");
  }
  println!();

  if report.total_fixed == 0 {
    println!("No fixes needed");
    return;
  }

  println!("Fixed {} version conflict(s):", report.total_fixed);
  println!();

  // Group by dependency
  let mut by_dep: std::collections::HashMap<String, Vec<&crate::lint::FixedVersion>> = std::collections::HashMap::new();
  for fix in &report.fixed {
    by_dep.entry(fix.dependency_name.clone()).or_default().push(fix);
  }

  for (dep_name, fixes) in by_dep.iter() {
    println!("📦 {}", dep_name);
    for fix in fixes {
      let filename = fix
        .manifest_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Cargo.toml");
      println!(
        "   {} ({})",
        fix
          .manifest_path
          .parent()
          .and_then(|p| p.file_name())
          .and_then(|n| n.to_str())
          .unwrap_or(""),
        filename
      );
      println!("      - {} = \"{}\"", fix.section, fix.from_version);
      println!("      + {} = \"{}\"", fix.section, fix.to_version);
      println!();
    }
  }

  if report.dry_run {
    println!("To apply these changes:");
    println!("  cargo rail lint versions --fix --apply");
  }
}

/// Run the lint manifest command
pub fn run_lint_manifest(ctx: &WorkspaceContext, json: bool, strict: bool) -> RailResult<()> {
  // Create context for checks
  let check_ctx = CheckContext {
    workspace_root: ctx.workspace_root().to_path_buf(),
    crate_name: None,
    thorough: false, // Manifest checks are never expensive
  };

  // Run manifest-specific checks
  let runner = create_manifest_runner();
  let results = runner.run_all(&check_ctx)?;

  // Output results
  if json {
    println!("{}", serde_json::to_string_pretty(&results)?);
  } else {
    print_manifest_results(&results);
  }

  // Determine exit code based on results
  let has_errors = results.iter().any(|r| !r.passed && r.severity == Severity::Error);
  let has_warnings = results.iter().any(|r| !r.passed && r.severity == Severity::Warning);

  // Exit with error in strict mode if any issues found
  if strict && (has_errors || has_warnings) {
    std::process::exit(1);
  } else if has_errors {
    // Always exit with error for hard errors
    std::process::exit(1);
  }

  Ok(())
}

fn print_manifest_results(results: &[crate::checks::CheckResult]) {
  let passed = results.iter().filter(|r| r.passed).count();
  let failed = results.iter().filter(|r| !r.passed).count();
  let errors = results
    .iter()
    .filter(|r| !r.passed && r.severity == Severity::Error)
    .count();
  let warnings = results
    .iter()
    .filter(|r| !r.passed && r.severity == Severity::Warning)
    .count();

  println!("📋 Manifest Quality Checks");
  println!();

  // Print summary
  if failed == 0 {
    println!("✅ All checks passed ({} total)", passed);
    println!();
  } else {
    println!("⚠️  {} issue(s) found", failed);
    if errors > 0 {
      println!("   🚫 {} error(s)", errors);
    }
    if warnings > 0 {
      println!("   ⚠️  {} warning(s)", warnings);
    }
    println!();
  }

  // Print each result
  for result in results {
    let icon = match (result.passed, result.severity) {
      (true, _) => "✅",
      (false, Severity::Error) => "🚫",
      (false, Severity::Warning) => "⚠️ ",
      (false, Severity::Info) => "ℹ️ ",
    };

    println!("{} {} - {}", icon, result.check_name, result.message);

    if let Some(suggestion) = &result.suggestion {
      println!("   💡 {}", suggestion);
    }

    if let Some(details) = &result.details {
      // Print specific details for manifest checks
      if let Some(issues) = details.get("issues")
        && let Some(arr) = issues.as_array()
      {
        for issue in arr {
          if let Some(obj) = issue.as_object()
            && let Some(crate_name) = obj.get("crate_name").and_then(|v| v.as_str())
          {
            println!("      - {}", crate_name);
          }
        }
      }

      if let Some(edition_usage) = details.get("edition_usage")
        && let Some(obj) = edition_usage.as_object()
      {
        for (edition, crates) in obj {
          if let Some(arr) = crates.as_array() {
            println!("      Edition {}: {} crate(s)", edition, arr.len());
          }
        }
      }
    }

    println!();
  }

  if failed > 0 {
    println!("Configure manifest policies in rail.toml:");
    println!("  [policy]");
    println!("  edition = \"2024\"");
    println!("  msrv = \"1.76.0\"");
    println!("  forbid_patch_replace = true");
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn test_module_exists() {
    // Ensure module compiles
  }
}
