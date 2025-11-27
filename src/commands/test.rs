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
  pub all: bool,
  /// Explain why tests are being run
  pub explain: bool,
  /// Prefer cargo-nextest if available
  pub prefer_nextest: bool,
  /// Additional arguments to pass to the test runner
  pub test_args: Vec<String>,
}

/// Run tests for affected crates
pub fn run_test(ctx: &WorkspaceContext, config: TestConfig) -> RailResult<()> {
  let analyzer = ChangeImpact::new(ctx);

  let base_ref = if let Some(ref s) = config.since {
    s.clone()
  } else {
    detect_default_base_ref(ctx.git.git())?
  };

  let impact = analyzer.analyze_changes(&base_ref, None)?;

  let test_targets = if config.all {
    ctx
      .cargo
      .metadata()
      .workspace_packages()
      .iter()
      .map(|p| p.name.to_string())
      .collect()
  } else {
    impact.minimal_test_set()
  };

  if test_targets.is_empty() {
    println!("no affected crates");
    return Ok(());
  }

  if config.explain {
    println!("changed files: {}", impact.changed_files.len());
    println!("rebuild: {}", if impact.requires_rebuild { "yes" } else { "no" });
    println!("retest: {}", if impact.requires_retest { "yes" } else { "no" });
    println!();

    if impact.categories.has_source_changes() {
      println!("source: {} files", impact.categories.source_files.len());
    }
    if impact.categories.has_test_changes() {
      println!("tests: {} files", impact.categories.test_files.len());
    }
    if impact.categories.has_example_changes() {
      println!("examples: {} files", impact.categories.example_files.len());
    }
    if impact.categories.has_config_changes() {
      println!("config: {} files", impact.categories.config_files.len());
    }
    if !impact.categories.build_scripts.is_empty() {
      println!("build scripts: {} files", impact.categories.build_scripts.len());
    }
    if !impact.categories.doc_files.is_empty() {
      println!("docs: {} files", impact.categories.doc_files.len());
    }
    println!();

    if !impact.direct_crates.is_empty() {
      println!("direct: {}", impact.direct_crates.len());
      for crate_name in &impact.direct_crates {
        println!("  {}", crate_name);
      }
    }
    if !impact.transitive_crates.is_empty() {
      println!("transitive: {}", impact.transitive_crates.len());
      for crate_name in &impact.transitive_crates {
        println!("  {}", crate_name);
      }
    }
    println!();
  }

  let runner = select_runner(config.prefer_nextest);

  eprintln!("testing {} crates ({}):", test_targets.len(), runner.name());
  for target in &test_targets {
    eprintln!("  {}", target);
  }

  let mut cmd = runner.build_command(&test_targets, &config.test_args);
  let status = cmd
    .status()
    .map_err(|e| crate::error::RailError::message(format!("{} failed: {}", runner.name(), e)))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  println!("\nall tests passed");

  Ok(())
}
