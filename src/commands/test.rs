//! Smart test runner - only test affected crates

use crate::error::RailResult;
use crate::git::detect_default_base_ref;
use crate::test::runner::select_runner;
use crate::workspace::{ChangeImpact, WorkspaceContext};

/// Configuration for test command
#[derive(Clone)]
pub struct TestConfig {
  /// Git ref to compare against (if None, auto-detect)
  pub since: Option<String>,
  /// Skip change detection and run all tests
  pub full: bool,
  /// Explain why tests are being run
  pub explain: bool,
  /// Prefer cargo-nextest if available
  pub prefer_nextest: bool,
  /// Additional arguments to pass to the test runner
  pub test_args: Vec<String>,
}

/// Run tests only for crates affected by changes since a given ref
pub fn run_test(ctx: &WorkspaceContext, config: TestConfig) -> RailResult<()> {
  let analyzer = ChangeImpact::new(ctx);

  // Auto-detect base ref if not provided
  let base_ref = if let Some(ref s) = config.since {
    s.clone()
  } else {
    detect_default_base_ref(ctx.git.git())?
  };

  // Analyze changes since the base ref (to working tree)
  let impact = analyzer.analyze_changes(&base_ref, None)?;

  // Get minimal test set
  let test_targets = if config.full {
    println!("⚡ Full mode active - running all tests");
    ctx
      .cargo
      .metadata()
      .list_crates()
      .iter()
      .map(|p| p.name.to_string())
      .collect()
  } else {
    impact.minimal_test_set()
  };

  if test_targets.is_empty() {
    println!("✓ No affected crates - all tests skipped");
    return Ok(());
  }

  if config.explain {
    println!("Change Impact Analysis:");
    println!("  Total files changed: {}", impact.changed_files.len());
    println!(
      "  Requires rebuild: {}",
      if impact.requires_rebuild { "yes" } else { "no" }
    );
    println!(
      "  Requires retest: {}",
      if impact.requires_retest { "yes" } else { "no" }
    );

    println!("\nFile Breakdown:");
    if impact.categories.has_source_changes() {
      println!("  - Source code: {} file(s)", impact.categories.source_files.len());
    }
    if impact.categories.has_test_changes() {
      println!("  - Tests: {} file(s)", impact.categories.test_files.len());
    }
    if impact.categories.has_example_changes() {
      println!("  - Examples: {} file(s)", impact.categories.example_files.len());
    }
    if impact.categories.has_config_changes() {
      println!("  - Config: {} file(s)", impact.categories.config_files.len());
    }
    if !impact.categories.build_scripts.is_empty() {
      println!("  - Build scripts: {} file(s)", impact.categories.build_scripts.len());
    }
    if !impact.categories.doc_files.is_empty() {
      println!("  - Documentation: {} file(s)", impact.categories.doc_files.len());
    }

    println!("\nAffected Crates:");
    println!("  - Direct: {} crate(s)", impact.direct_crates.len());
    if !impact.direct_crates.is_empty() {
      for crate_name in &impact.direct_crates {
        println!("    • {}", crate_name);
      }
    }
    println!("  - Transitive: {} crate(s)", impact.transitive_crates.len());
    if !impact.transitive_crates.is_empty() {
      for crate_name in &impact.transitive_crates {
        println!("    • {}", crate_name);
      }
    }
    println!();
  }

  // Select test runner
  let runner = select_runner(config.prefer_nextest);

  println!(
    "Running tests for {} affected crate(s) using {}:",
    test_targets.len(),
    runner.name()
  );
  for target in &test_targets {
    println!("  • {}", target);
  }
  println!();

  // Build and run command
  let mut cmd = runner.build_command(&test_targets, &config.test_args);
  let status = cmd
    .status()
    .map_err(|e| crate::error::RailError::message(format!("Failed to run {}: {}", runner.name(), e)))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  println!("\n✓ All affected tests passed");

  Ok(())
}
