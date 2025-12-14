//! Smart test runner - only test affected crates

use crate::change_detection::ChangeClassifier;
use crate::error::RailResult;
use crate::git::detect_default_base_ref;
use crate::progress;
use crate::test::runner::select_runner;
use crate::workspace::{ChangeImpact, WorkspaceContext};

/// Configuration for test command
#[derive(Clone)]
pub struct TestConfig {
  /// Git ref to compare against (if None, auto-detect)
  pub since: Option<String>,
  /// Use merge-base with default branch (better for feature branches)
  pub merge_base: bool,
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
  // When --all is used, skip change detection entirely (performance optimization)
  let (test_targets, impact, rebuild_all) = if config.all {
    let targets: Vec<String> = ctx
      .cargo
      .metadata()
      .workspace_packages()
      .iter()
      .map(|p| p.name.to_string())
      .collect();
    (targets, None, false)
  } else {
    let analyzer = ChangeImpact::new(ctx);

    let base_ref = if config.merge_base {
      // Use merge-base with auto-detected default branch
      let default_branch = detect_default_base_ref(ctx.git.git())?;
      ctx.git.git().get_merge_base(&default_branch, "HEAD")?
    } else if let Some(ref s) = config.since {
      s.clone()
    } else {
      detect_default_base_ref(ctx.git.git())?
    };

    let impact = analyzer.analyze_changes(&base_ref, None)?;

    // Apply change-detection config (Layer 3 classification)
    // This checks for infrastructure patterns that require full rebuild
    let classifier = ctx
      .config
      .as_ref()
      .map(|c| ChangeClassifier::new(&c.change_detection))
      .unwrap_or_else(ChangeClassifier::default_config);

    let file_paths: Vec<String> = impact
      .changed_files
      .iter()
      .map(|(path, _status)| path.to_string_lossy().to_string())
      .collect();
    let file_refs: Vec<&str> = file_paths.iter().map(|s| s.as_str()).collect();
    let classification = classifier.classify(&file_refs);

    // If infrastructure files changed, test all crates
    let (targets, rebuild_all) = if classification.rebuild_all {
      let all_targets: Vec<String> = ctx
        .cargo
        .metadata()
        .workspace_packages()
        .iter()
        .map(|p| p.name.to_string())
        .collect();
      (all_targets, true)
    } else {
      (impact.minimal_test_set(), false)
    };

    (targets, Some(impact), rebuild_all)
  };

  if test_targets.is_empty() {
    println!("no affected crates");
    return Ok(());
  }

  if config.explain {
    if let Some(ref impact) = impact {
      println!("changed files: {}", impact.changed_files.len());
      println!("rebuild: {}", if impact.requires_rebuild { "yes" } else { "no" });
      println!("retest: {}", if impact.requires_retest { "yes" } else { "no" });
      if rebuild_all {
        println!("infrastructure: yes (testing all crates)");
      }
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
    } else {
      // --all mode: no change detection performed
      println!("mode: all (change detection skipped)");
      println!("crates: {}", test_targets.len());
      println!();
    }
  }

  let runner = select_runner(config.prefer_nextest);

  progress!("testing {} crates ({}):", test_targets.len(), runner.name());
  for target in &test_targets {
    progress!("  {}", target);
  }

  let mut cmd = runner.build_command(&test_targets, &config.test_args);
  let status = cmd
    .status()
    .map_err(|e| crate::error::RailError::message(format!("{} failed: {}", runner.name(), e)))?;

  if !status.success() {
    return Err(crate::error::RailError::ExitWithCode {
      code: status.code().unwrap_or(1),
    });
  }

  println!("\nall tests passed");

  Ok(())
}
